use axum_server::tls_rustls::RustlsConfig;
use base64::{engine::general_purpose::STANDARD as base64_std, Engine as _};
use futures::StreamExt;
use k8s_openapi::api::admissionregistration::v1::MutatingWebhookConfiguration;
use k8s_openapi::api::core::v1::Secret;
use k8s_openapi::ByteString;
use kube::{api::{Api, Patch, PatchParams, PostParams}, Client};
use kube::runtime::watcher::{watcher, Config as WatcherConfig, Event};
use rcgen::{CertificateParams, DistinguishedName, DnType, SanType};
use std::collections::BTreeMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tracing::{error, info, warn};

const SECRET_NAME: &str = "rustjack-tls";

pub fn generate_certs(svc_name: &str, namespace: &str) -> (Vec<u8>, Vec<u8>) {
    info!("Generating new 12-hour TLS certificates...");
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
    params.not_after = now + time::Duration::hours(12);

    let cert = rcgen::Certificate::from_params(params).unwrap();
    (cert.serialize_pem().unwrap().into_bytes(), cert.serialize_private_key_pem().into_bytes())
}

pub fn get_cert_expiry(cert_pem: &[u8]) -> u64 {
    if let Ok((_, pem)) = x509_parser::pem::parse_x509_pem(cert_pem) {
        if let Ok((_, cert)) = x509_parser::parse_x509_certificate(&pem.contents) {
            return cert.tbs_certificate.validity.not_after.timestamp() as u64;
        }
    }
    0
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

pub async fn initialize_tls(client: &Client, svc_name: &str, namespace: &str, webhook_name: &str) -> (Vec<u8>, Vec<u8>, u64) {
    let secrets: Api<Secret> = Api::namespaced(client.clone(), namespace);

    match secrets.get(SECRET_NAME).await {
        Ok(secret) => {
            let data = secret.data.unwrap_or_default();
            let cert = data.get("tls.crt").unwrap().0.clone();
            let key = data.get("tls.key").unwrap().0.clone();
            let expiry = get_cert_expiry(&cert);
            info!("Loaded existing TLS from Secret. Expiry timestamp: {}", expiry);

            patch_webhook_config(client, &cert, webhook_name).await;

            (cert, key, expiry)
        }
        Err(_) => {
            let (cert, key) = generate_certs(svc_name, namespace);
            let mut data = BTreeMap::new();
            data.insert("tls.crt".to_string(), ByteString(cert.clone()));
            data.insert("tls.key".to_string(), ByteString(key.clone()));

            let secret = Secret {
                metadata: k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta {
                    name: Some(SECRET_NAME.to_string()),
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
                    let secret = secrets.get(SECRET_NAME).await.unwrap();
                    let data = secret.data.unwrap();
                    let cert = data.get("tls.crt").unwrap().0.clone();
                    let key = data.get("tls.key").unwrap().0.clone();
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
    initial_tls: (Vec<u8>, Vec<u8>, u64),
) {
    let secrets: Api<Secret> = Api::namespaced(client.clone(), &namespace);
    let watcher_config = WatcherConfig::default().fields(&format!("metadata.name={}", SECRET_NAME));
    let watcher_stream = watcher(secrets.clone(), watcher_config);
    tokio::pin!(watcher_stream);

    let (mut current_cert, mut current_key, mut current_expiry) = initial_tls;

    loop {
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
        let renew_threshold = 10800; 

        let time_to_sleep = if current_expiry > now + renew_threshold {
            current_expiry - now - renew_threshold
        } else {
            0
        };

        tokio::select! {
            Some(event) = watcher_stream.next() => {
                if let Ok(Event::Applied(secret)) = event {
                    if let Some(data) = secret.data {
                        if let Some(cert_bs) = data.get("tls.crt") {
                            if let Some(key_bs) = data.get("tls.key") {
                                let new_cert = cert_bs.0.clone();
                                let new_key = key_bs.0.clone();

                                if new_cert != current_cert {
                                    info!("Watch API Event: Loading new TLS certificates into RAM...");
                                    current_expiry = get_cert_expiry(&new_cert);
                                    current_cert = new_cert.clone();
                                    current_key = new_key.clone();
                                    tls_config.reload_from_pem(new_cert, new_key).await;
                                }
                            }
                        }
                    }
                }
            },
            _ = tokio::time::sleep(Duration::from_secs(time_to_sleep)) => {
                warn!("TLS certificate is expiring in < 3 hours. Initiating auto-renewal...");
                let (new_cert, new_key) = generate_certs(&svc_name, &namespace);

                let patch_json = serde_json::json!({
                    "data": {
                        "tls.crt": base64_std.encode(&new_cert),
                        "tls.key": base64_std.encode(&new_key)
                    }
                });

                let patch = Patch::Merge(patch_json);
                match secrets.patch(SECRET_NAME, &PatchParams::default(), &patch).await {
                    Ok(_) => {
                        info!("Successfully renewed TLS Secret. Triggering K8s Watch API for peers.");
                        patch_webhook_config(&client, &new_cert, &webhook_name).await;
                    }
                    Err(e) => {
                        let err_str = e.to_string();
                        if err_str.contains("NotFound") || err_str.contains("404") {
                            warn!("WARN: Secret {} is missing! Falling back to recreate it.", SECRET_NAME);
                            
                            let mut data = BTreeMap::new();
                            data.insert("tls.crt".to_string(), ByteString(new_cert.clone()));
                            data.insert("tls.key".to_string(), ByteString(new_key.clone()));

                            let secret = Secret {
                                metadata: k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta {
                                    name: Some(SECRET_NAME.to_string()),
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
                            info!("Another replica already renewed the Secret or error occurred.");
                        }
                    }
                }
            }
        }
    }
}