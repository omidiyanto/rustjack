# 🦀 RustJack - Ultimate Certificate Authority Injector for Kubernetes Pods 💉

<div align="center">
    <a href="[https://sonarcloud.io/summary/new_code?id=omidiyanto_rustjack](https://sonarcloud.io/summary/new_code?id=omidiyanto_rustjack)">
        <img src="[https://sonarcloud.io/api/project_badges/measure?project=omidiyanto_rustjack&metric=alert_status](https://sonarcloud.io/api/project_badges/measure?project=omidiyanto_rustjack&metric=alert_status)" alt="Quality Gate Status">
    </a>
    <br><br>
    <img src="https://img.shields.io/badge/Rust-red?style=for-the-badge&logo=rust&logoColor=#E57324">
    <img src="https://img.shields.io/badge/kubernetes-blue?style=for-the-badge&logo=kubernetes&logoColor=white">
    <img src="https://img.shields.io/badge/docker-green.svg?style=for-the-badge&logo=docker&logoColor=black">
    <img src="https://img.shields.io/badge/helm-black?style=for-the-badge&logo=helm&logoColor=white">
</div>
<br>

> **A blazing fast, zero-allocation, stateless, and cloud-native event-driven Mutating Admission Webhook for CA Injection in Kubernetes.**

RustJack allows off-the-shelf deployments to run in enterprise or air-gapped clusters with custom Certificate Authorities (CAs) **without modifying container images**. Say goodbye to the manual, repetitive process of `ADD yourca.crt ...` and `RUN update-ca-certificates` in your Dockerfiles.

---

## 🎯 The Pain of Custom CAs (Why You Need RustJack)

If you work in an enterprise, air-gapped, or highly regulated environment, you already know the nightmare of Deep Packet Inspection (DPI), SSL interception, and custom root authorities. Before RustJack, SRE/Platform/DevOps Engineers had to choose their poison:

* 🚫 **The Dockerfile Anti-Pattern (Image Bloat):** Forking and rebuilding every upstream Open Source image (like Alpine, Node, or Python) just to inject `corporate-ca.crt`. It breaks your software supply chain, slows down CI/CD pipelines, and creates a maintenance nightmare every time an upstream image updates.
* 🚫 **The YAML Manifest Hell:** Manually hardcoding `volumes`, `volumeMounts`, and environment variables (`SSL_CERT_FILE`, `NODE_EXTRA_CA_CERTS`) into hundreds of Deployments and Helm charts. A single typo brings down the application with `x509: certificate signed by unknown authority`.
* 🚫 **The InitContainer Tax (Boilerplate Burden):** Forcing developers to inject `initContainers` or `sidecars` into every Pod just to fetch and mount a certificate. It severely bloats your YAML, slows down Pod startup times, wastes compute resources, and shifts infrastructure responsibilities onto the application team.
* 🚫 **Bloated Legacy Webhooks:** Using traditional mutating webhooks written in memory-heavy languages that consume 50MB+ of RAM at idle, introduce API Server latency during Pod scheduling, and rely on a fragile maze of third-party cert-managers just to keep their own webhook TLS alive.

**RustJack is the ultimate cure.** It decouples the certificate trust layer from your application layer. It automates the injection cleanly, instantly, and with zero operational overhead, allowing your developers to focus on code, not certificates.

---

## 🏛️ The RustJack Philosophy

**RustJack is not just another Kubernetes webhook**, it is an architectural statement. Built with extreme Site Reliability Engineering (SRE) principles, it completely redefines how infrastructure components should behave:

* **Zero-Allocation Pipeline:** Processes Kubernetes JSON patches using pointer references (`&str`). No unnecessary heap memory allocations, resulting in a flat **1 MiB RAM** usage and **< 1ms latency**, even when processing hundreds of concurrent Pod creations.
* **True Stateless High Availability (HA):** Powered by the Kubernetes Watch API. Multiple RustJack replicas sync their TLS states entirely in-memory across the cluster without *Split-Brain* or API Server polling.
* **Zero Trust "Short-Lived" TLS:** Discards external cert-managers for its internal webhook. RustJack generates its own cryptographic keys in-memory with a **12-hour lifespan**, auto-renewing seamlessly at the 9-hour mark. Reduced blast radius, extreme security.
* **Distroless & Immutable:** Packaged in Google's `cc-debian12:nonroot` Distroless image. No shell (`/bin/sh`), no root access, and constrained by Strict Least-Privilege RBAC.
* **Idempotent Self-Healing:** Chaos-tested against node failures and aggressive secret deletions. RustJack gracefully handles `SIGTERM` and recreates missing cryptographic states instantly without CPU spikes.

---

## ✨ How It Works

When a Pod with the `rustjack.io/inject: "true"` label is submitted to the Kubernetes API, RustJack intercepts the request and instantly injects the required CA trust chain before the Pod is scheduled:

1.  **Injects a Secret Volume:** Mounts your custom CA bundle into all containers (including `initContainers`).
2.  **Exports Environment Variables:** Automatically configures standard trust paths (`SSL_CERT_FILE`, `REQUESTS_CA_BUNDLE`, `NODE_EXTRA_CA_CERTS`, etc.) pointing to the injected CA file.

Your applications instantly trust the corporate CA—zero code changes required.

---

## 🚀 Installation

### 1. Deploy via Helm
```bash
helm repo add rustjack https://omidiyanto.github.io/rustjack/
helm repo update

helm upgrade --install rustjack-cainjector rustjack/rustjack-cainjector \
  --namespace cert-manager \
  --create-namespace \
  --wait
```

### 2. Prepare Your CA Secret
Ensure your custom CA bundle is stored as a standard Kubernetes `Opaque` Secret in the namespace where your application resides.

---

## 🔗 Enterprise Ecosystem: CA Distribution (Optional)

**Architectural Note:** RustJack is **100% independent**. It does not strictly require `cert-manager`, `trust-manager`, or any third-party operators to function. If you manually create a standard Kubernetes `Opaque` Secret containing your `ca.crt` via `kubectl create secret...`, RustJack will happily inject it.

However, in a large-scale enterprise cluster with dozens of namespaces, manually copying CA Secrets is an operational anti-pattern. 

### The "trust-manager" Synergy
For true enterprise automation, we highly recommend pairing RustJack with [trust-manager](https://cert-manager.io/docs/trust/trust-manager/) (part of the `cert-manager` ecosystem). 

1. Install the cert-manager & trust-manager stack
```bash
helm repo add jetstack https://charts.jetstack.io --force-update

helm install \
  cert-manager jetstack/cert-manager \
  --namespace cert-manager \
  --create-namespace \
  --set crds.enabled=true

helm upgrade trust-manager jetstack/trust-manager \
  --install \
  --namespace cert-manager \
  --wait \
  --set secretTargets.enabled=true \
  --set secretTargets.authorizedSecretsAll=true
```

2. You define your corporate CA once using a `Bundle` Custom Resource.
```yaml
apiVersion: trust.cert-manager.io/v1alpha1
kind: Bundle
metadata:
  # This name will be referenced in your pod's annotation
  name: my-root-ca
spec:
  sources:
    # (Optional) Include the system's default CAs
    - useDefaultCAs: true
    # Paste your custom CA certificate here
    - inLine: |
        -----BEGIN CERTIFICATE-----
        MIIDNTC..................
        -----END CERTIFICATE-----
  target:
    # Configuration to create a Secret
    secret:
      key: "ca.crt"
```

3. `trust-manager` automatically replicates this CA as a standard Kubernetes `Secret` into every namespace across your cluster.
4. **RustJack** natively and seamlessly mounts these replicated Secrets into your application Pods on-the-fly.


With this setup, your infrastructure is entirely automated: `cert-manager` handles rotation, `trust-manager` handles namespace distribution, and **RustJack** handles the zero-touch injection into the Pods.

---

## 💡 Usage Guide

RustJack operates in **Strict Mode**. To prevent unnecessary webhook invocations and protect cluster performance, you **MUST** provide both the specific trigger label and the configuration annotation.

### Configuration API

| Scope | Key | Description |
| :--- | :--- | :--- |
| **Label** | `rustjack.io/inject: "true"` | **[REQUIRED]** Instructs the Kube-apiserver to route this Pod to the RustJack webhook. |
| **Annotation** | `rustjack.io/ca-secret` | **[REQUIRED]** The exact name of the Kubernetes Secret containing your `ca.crt`. |
| **Annotation** | `rustjack.io/mount-path` | *[Optional]* The directory where the CA will be mounted. **Default:** `/ssl`. |
| **Annotation** | `rustjack.io/extra-envs` | *[Optional]* Comma-separated list of additional environment variables to inject. |

### Example: Zero-Touch Deployment

```yaml
apiVersion: apps/v1
kind: Deployment
metadata:
  name: my-secure-app
  namespace: default
spec:
  replicas: 3
  selector:
    matchLabels:
      app: my-secure-app
  template:
    metadata:
      labels:
        app: my-secure-app
        # 👇 1. THE TRIGGER LABEL 👇
        rustjack.io/inject: "true"
      annotations:
        # 👇 2. DEFINE CA SECRET NAME TO BE INJECTED 👇
        rustjack.io/ca-secret: "my-root-ca"
    spec:
      containers:
      - name: main-app
        image: alpine:latest
        command: ["sleep", "infinity"]
```

### Verification
Once deployed, verify the blazing-fast injection:
```bash
# Check the RustJack logs
kubectl logs -l app.kubernetes.io/name=rustjack-cainjector -n cert-manager

# Verify the application Pod
kubectl exec -it deploy/my-secure-app -- env | grep "SSL\|CUSTOM"
kubectl exec -it deploy/my-secure-app -- ls -la /ssl/ca.crt
```

---

## 📊 Performance Benchmarks

Tested on a production RKE2 cluster with 30 concurrent Pod scaling operations:

* **Memory Footprint:** `~ 1.0 MiB` (Flat line, immune to garbage collection spikes)
* **CPU Usage:** `~ 1m` (0.001 cores at peak concurrent load)
* **API Latency:** `< 1 ms`
* **High Availability:** Active-Active Topology Spread via Kubernetes Watch API

---

## 🤝 Contributing

We ❤️ DevOps, SREs, Platform Engineers, and Rustaceans! 
Whether you want to optimize a nanosecond of latency or improve documentation, your PR is welcome.

1. **Fork** the repository.
2. **Clone** locally and create a feature branch (`git checkout -b feature/nanosecond-optimization`).
3. **Commit** your changes following conventional commits.
4. **Push** and open a Pull Request.

> *"If it allocates heap unnecessarily, it's a bug."* – The RustJack Team.
