use axum_server::tls_rustls::RustlsConfig;
use base64::{engine::general_purpose::STANDARD as base64_std, Engine as _};
use futures::StreamExt;
use k8s_openapi::api::admissionregistration::v1::MutatingWebhookConfiguration;
use k8s_openapi::api::core::v1::Secret;
use k8s_openapi::ByteString;
use kube::{api::{Api, Patch, PatchParams, PostParams}, Client};
use kube::runtime::watcher::{watcher, Config as WatcherConfig, Event};
use rand::Rng;
use rcgen::{CertificateParams, DistinguishedName, DnType, SanType};
use std::collections::BTreeMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tracing::{error, info, warn};

pub fn generate_certs(svc_name: &str, namespace: &str) -> Result<(Vec<u8>, Vec<u8>), String> {
    info!("Generating new 15-minute TLS certificates...");
    let mut params = CertificateParams::default();
    let mut dn = DistinguishedName::new();
    dn.push(DnType::CommonName, svc_name);
    params.distinguished_name = dn;

    params.subject_alt_names = vec![
        SanType::DnsName(format!("{svc_name}.{namespace}.svc")),
        SanType::DnsName(format!("{svc_name}.{namespace}.svc.cluster.local")),
    ];

    let now = time::OffsetDateTime::now_utc();
    params.not_before = now;
    params.not_after = now + time::Duration::minutes(15);

    let cert = rcgen::Certificate::from_params(params)
        .map_err(|e| format!("Failed to create certificate params: {}", e))?;

    let cert_pem = cert.serialize_pem()
        .map_err(|e| format!("Failed to serialize certificate PEM: {}", e))?
        .into_bytes();

    let key_pem = cert.serialize_private_key_pem().into_bytes();

    Ok((cert_pem, key_pem))
}

pub fn get_cert_expiry(cert_pem: &[u8]) -> u64 {
    if let Ok((_, pem)) = x509_parser::pem::parse_x509_pem(cert_pem) {
        if let Ok((_, cert)) = x509_parser::parse_x509_certificate(&pem.contents) {
            return cert.tbs_certificate.validity.not_after.timestamp() as u64;
        }
    }
    0
}

fn extract_tls_from_secret(data: &BTreeMap<String, ByteString>) -> Option<(Vec<u8>, Vec<u8>)> {
    let cert = data.get("tls.crt")?.0.clone();
    let key = data.get("tls.key")?.0.clone();
    Some((cert, key))
}

pub async fn patch_webhook_config(client: &Client, ca_pem: &[u8], webhook_name: &str) {
    let api: Api<MutatingWebhookConfiguration> = Api::all(client.clone());
    let b64_ca = base64_std.encode(ca_pem);

    let patch_json = serde_json::json!({
        "webhooks": [{
            "name": format!("mutate.{}.svc", webhook_name),
            "clientConfig": {
                "caBundle": b64_ca
            }
        }]
    });

    let patch = Patch::Strategic(patch_json);
    if let Err(e) = api.patch(webhook_name, &PatchParams::default(), &patch).await {
        error!("Failed to update Webhook caBundle: {}", e);
    } else {
        info!("Successfully updated MutatingWebhookConfiguration caBundle");
    }
}

pub async fn initialize_tls(client: &Client, svc_name: &str, namespace: &str, webhook_name: &str, secret_name: &str) -> (Vec<u8>, Vec<u8>, u64) {
    let secrets: Api<Secret> = Api::namespaced(client.clone(), namespace);

    match secrets.get(secret_name).await {
        Ok(secret) => {
            let data = secret.data.unwrap_or_default();
            match extract_tls_from_secret(&data) {
                Some((cert, key)) => {
                    let expiry = get_cert_expiry(&cert);
                    info!("Loaded existing TLS from Secret. Expiry timestamp: {}", expiry);
                    patch_webhook_config(client, &cert, webhook_name).await;
                    (cert, key, expiry)
                }
                None => {
                    warn!("Secret '{}' exists but missing tls.crt/tls.key fields. Regenerating...", secret_name);
                    generate_and_store_secret(client, &secrets, svc_name, namespace, webhook_name, secret_name).await
                }
            }
        }
        Err(_) => {
            generate_and_store_secret(client, &secrets, svc_name, namespace, webhook_name, secret_name).await
        }
    }
}

async fn generate_and_store_secret(
    client: &Client,
    secrets: &Api<Secret>,
    svc_name: &str,
    namespace: &str,
    webhook_name: &str,
    secret_name: &str,
) -> (Vec<u8>, Vec<u8>, u64) {
    let (cert, key) = match generate_certs(svc_name, namespace) {
        Ok(pair) => pair,
        Err(e) => {
            error!("FATAL: Failed to generate TLS certificates: {}", e);
            panic!("Cannot start without valid TLS certificates: {}", e);
        }
    };

    let mut data = BTreeMap::new();
    data.insert("tls.crt".to_string(), ByteString(cert.clone()));
    data.insert("tls.key".to_string(), ByteString(key.clone()));

    let secret = Secret {
        metadata: k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta {
            name: Some(secret_name.to_string()),
            ..Default::default()
        },
        data: Some(data),
        ..Default::default()
    };

    match secrets.create(&PostParams::default(), &secret).await {
        Ok(_) => {
            info!("Successfully executed Atomic Create for TLS Secret");
            patch_webhook_config(client, &cert, webhook_name).await;
            let expiry = get_cert_expiry(&cert);
            (cert, key, expiry)
        }
        Err(_) => {
            info!("Lost Atomic Create race. Reading from winner...");
            match secrets.get(secret_name).await {
                Ok(secret) => {
                    let data = secret.data.unwrap_or_default();
                    match extract_tls_from_secret(&data) {
                        Some((cert, key)) => {
                            let expiry = get_cert_expiry(&cert);
                            (cert, key, expiry)
                        }
                        None => {
                            error!("Winner's Secret is also missing tls.crt/tls.key. Using locally generated certs.");
                            let expiry = get_cert_expiry(&cert);
                            (cert, key, expiry)
                        }
                    }
                }
                Err(e) => {
                    error!("Failed to read Secret after losing create race: {}. Using locally generated certs.", e);
                    let expiry = get_cert_expiry(&cert);
                    (cert, key, expiry)
                }
            }
        }
    }
}

pub async fn start_ha_tls_manager(
    client: Client,
    tls_config: RustlsConfig,
    namespace: String,
    svc_name: String,
    webhook_name: String,
    secret_name: String,
    initial_tls: (Vec<u8>, Vec<u8>, u64),
) {
    let secrets: Api<Secret> = Api::namespaced(client.clone(), &namespace);
    let watcher_config = WatcherConfig::default().fields(&format!("metadata.name={}", secret_name));
    let watcher_stream = watcher(secrets.clone(), watcher_config);
    tokio::pin!(watcher_stream);

    let (mut current_cert, mut _current_key, mut current_expiry) = initial_tls;

    loop {
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
        let renew_threshold = 600; 

        let time_to_sleep = if current_expiry > now + renew_threshold {
            current_expiry - now - renew_threshold
        } else {
            0
        };

        tokio::select! {
            Some(event) = watcher_stream.next() => {
                match event {
                    Ok(Event::Applied(secret)) => {
                        if let Some(data) = secret.data {
                            match extract_tls_from_secret(&data) {
                                Some((new_cert, new_key)) => {
                                    if new_cert != current_cert {
                                        info!("Watch API Event: Loading new TLS certificates into RAM...");
                                        current_expiry = get_cert_expiry(&new_cert);
                                        current_cert = new_cert.clone();
                                        _current_key = new_key.clone();
                                        if let Err(e) = tls_config.reload_from_pem(new_cert, new_key).await {
                                            error!("Failed to reload TLS config from PEM: {:?}. Server continues with previous certificates.", e);
                                        }
                                    }
                                }
                                None => {
                                    warn!("Watch event received but Secret missing tls.crt/tls.key. Ignoring event.");
                                }
                            }
                        }
                    }
                    Ok(_) => {}
                    Err(e) => {
                        warn!("Watcher error: {}. Stream will auto-recover.", e);
                    }
                }
            },
            _ = tokio::time::sleep(Duration::from_secs(time_to_sleep)) => {
                // Randomized Jitter (0-60s) to prevent Thundering Herd in HA
                let jitter = rand::thread_rng().gen_range(0..=60);
                info!("TLS certificate expiring in < 10 minutes. Waiting {}s jitter before renewal...", jitter);
                tokio::time::sleep(Duration::from_secs(jitter)).await;

                // Re-check Secret after jitter — another replica may have already renewed
                match secrets.get(&secret_name).await {
                    Ok(secret) => {
                        if let Some(data) = secret.data {
                            if let Some((existing_cert, _)) = extract_tls_from_secret(&data) {
                                let existing_expiry = get_cert_expiry(&existing_cert);
                                let now_after_jitter = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
                                if existing_expiry > now_after_jitter + renew_threshold {
                                    info!("Another replica already renewed the TLS Secret. Skipping renewal.");
                                    current_expiry = existing_expiry;
                                    continue;
                                }
                            }
                        }
                    }
                    Err(e) => {
                        warn!("Failed to re-check Secret after jitter: {}. Proceeding with renewal.", e);
                    }
                }

                let (new_cert, new_key) = match generate_certs(&svc_name, &namespace) {
                    Ok(pair) => pair,
                    Err(e) => {
                        error!("Failed to generate new TLS certificates during renewal: {}. Will retry next cycle.", e);
                        continue;
                    }
                };

                let patch_json = serde_json::json!({
                    "data": {
                        "tls.crt": base64_std.encode(&new_cert),
                        "tls.key": base64_std.encode(&new_key)
                    }
                });

                let patch = Patch::Merge(patch_json);
                match secrets.patch(&secret_name, &PatchParams::default(), &patch).await {
                    Ok(_) => {
                        info!("Successfully renewed TLS Secret. Triggering K8s Watch API for peers.");
                        patch_webhook_config(&client, &new_cert, &webhook_name).await;
                    }
                    Err(e) => {
                        let err_str = e.to_string();
                        if err_str.contains("NotFound") || err_str.contains("404") {
                            warn!("Secret '{}' is missing! Falling back to recreate it.", secret_name);
                            
                            let mut data = BTreeMap::new();
                            data.insert("tls.crt".to_string(), ByteString(new_cert.clone()));
                            data.insert("tls.key".to_string(), ByteString(new_key.clone()));

                            let secret = Secret {
                                metadata: k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta {
                                    name: Some(secret_name.to_string()),
                                    ..Default::default()
                                },
                                data: Some(data),
                                ..Default::default()
                            };

                            if let Ok(_) = secrets.create(&PostParams::default(), &secret).await {
                                info!("Successfully executed Fallback Atomic Create for TLS Secret.");
                                patch_webhook_config(&client, &new_cert, &webhook_name).await;
                            } else {
                                info!("Lost Fallback Atomic Create race. Another replica handled it.");
                            }
                        } else {
                            error!("Failed to patch Secret during renewal: {}", e);
                        }
                    }
                }
            }
        }
    }
}