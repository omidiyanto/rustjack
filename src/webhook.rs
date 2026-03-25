use axum::Json;
use base64::{engine::general_purpose::STANDARD as base64_std, Engine as _};
use k8s_openapi::api::core::v1::{Container, Pod};
use serde_json::{json, Value};
use tracing::{debug, info};

pub async fn mutate_handler(Json(body): Json<Value>) -> Json<Value> {
    let req = match body.get("request") {
        Some(r) => r,
        None => return Json(json!({"response": {"allowed": true}}))
    };

    let uid = req.get("uid").and_then(|u| u.as_str()).unwrap_or("");
    let namespace = req.get("namespace").and_then(|n| n.as_str()).unwrap_or("unknown-ns");

    let pod: Pod = match serde_json::from_value(req.get("object").unwrap_or(&json!(null)).clone()) {
        Ok(p) => p,
        Err(_) => return Json(json!({"response": {"uid": uid, "allowed": true}}))
    };

    let annotations = pod.metadata.annotations.unwrap_or_default();
    let secret_name = match annotations.get("rustjack.io/ca-secret") {
        Some(name) => name,
        None => {
            debug!("Skipping: No rustjack.io/ca-secret annotation found");
            return Json(json!({"response": {"uid": uid, "allowed": true}}))
        }
    };

    let name = pod.metadata.name.as_deref();
    let gen_name = pod.metadata.generate_name.as_deref();

    match (name, gen_name) {
        (Some(n), _) => {
            info!("Injecting CA from Secret '{}' into {}/{}", secret_name, namespace, n);
        }
        (None, Some(g)) => {
            info!("Injecting CA from Secret '{}' into {}/{}<generated>", secret_name, namespace, g);
        }
        (None, None) => {
            info!("Injecting CA from Secret '{}' into {}/unknown-pod", secret_name, namespace);
        }
    }

    let mount_path = annotations.get("rustjack.io/mount-path").map(|s| s.as_str()).unwrap_or("/ssl");
    let ca_file = format!("{}/ca.crt", mount_path);
    let mut patch = Vec::new();

    if let Some(spec) = pod.spec {
        if spec.volumes.is_none() {
            patch.push(json!({"op": "add", "path": "/spec/volumes", "value": []}));
        }

        patch.push(json!({
            "op": "add",
            "path": "/spec/volumes/-",
            "value": {
                "name": "rustjack-injected-ssl",
                "secret": {"secretName": secret_name}
            }
        }));

        let extra_envs_str = annotations.get("rustjack.io/extra-envs").map(|s| s.as_str()).unwrap_or("");
        let extra_envs: Vec<&str> = extra_envs_str.split(',').filter(|s| !s.is_empty()).collect();

        let mut patch_containers = |containers: &Vec<Container>, path_prefix: &str| {
            for (i, container) in containers.iter().enumerate() {
                if container.volume_mounts.is_none() {
                    patch.push(json!({"op": "add", "path": format!("{}/{}/volumeMounts", path_prefix, i), "value": []}));
                }

                patch.push(json!({
                    "op": "add", "path": format!("{}/{}/volumeMounts/-", path_prefix, i),
                    "value": { "name": "rustjack-injected-ssl", "mountPath": mount_path, "readOnly": true }
                }));

                let mut envs_to_add = vec![
                    json!({"name": "SSL_CERT_FILE", "value": &ca_file}),
                    json!({"name": "REQUESTS_CA_BUNDLE", "value": &ca_file}),
                    json!({"name": "NODE_EXTRA_CA_CERTS", "value": &ca_file}),
                ];

                for extra in extra_envs.iter() {
                    envs_to_add.push(json!({"name": extra, "value": &ca_file}));
                }

                if container.env.is_none() {
                    patch.push(json!({"op": "add", "path": format!("{}/{}/env", path_prefix, i), "value": []}));
                }

                for env in envs_to_add {
                    patch.push(json!({"op": "add", "path": format!("{}/{}/env/-", path_prefix, i), "value": env}));
                }
            }
        };

        patch_containers(&spec.containers, "/spec/containers");

        if let Some(init_containers) = &spec.init_containers {
            patch_containers(init_containers, "/spec/initContainers");
        }
    }

    let patch_json = serde_json::to_string(&patch).unwrap();
    let patch_b64 = base64_std.encode(patch_json);

    Json(json!({
        "apiVersion": "admission.k8s.io/v1",
        "kind": "AdmissionReview",
        "response": {
            "uid": uid,
            "allowed": true,
            "patch": patch_b64,
            "patchType": "JSONPatch"
        }
    }))
}