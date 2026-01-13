# NG Gateway Helm Chart

This Helm chart deploys NG Gateway on a Kubernetes cluster.

## Prerequisites

- Kubernetes 1.19+
- Helm 3.0+
- PV provisioner support in the underlying infrastructure (if using persistent storage)

## Installation

### Add the Helm repository (if applicable)

```bash
helm repo add ng https://charts.ng.com
helm repo update
```

### Install the chart

```bash
# Install with default values
helm install my-gateway ./ng-gateway

# Install with custom values
helm install my-gateway ./ng-gateway -f my-values.yaml

# Install with custom values and set parameters
helm install my-gateway ./ng-gateway \
  --set gateway.image.tag=v1.0.0 \
  --set gateway.service.type=NodePort
```

## Configuration

The following table lists the configurable parameters and their default values.

### Global Parameters

| Parameter | Description | Default |
|-----------|-------------|---------|
| `global.imageRegistry` | Global Docker image registry | `""` |
| `global.imagePullPolicy` | Global image pull policy | `IfNotPresent` |
| `global.storageClass` | Global storage class for PVCs | `""` |

### Gateway Parameters

| Parameter | Description | Default |
|-----------|-------------|---------|
| `gateway.enabled` | Enable gateway deployment | `true` |
| `gateway.image.repository` | Gateway image repository | `ng-gateway` |
| `gateway.image.tag` | Gateway image tag | `latest` |
| `gateway.replicaCount` | Number of gateway replicas | `1` |
| `gateway.service.type` | Gateway service type | `ClusterIP` |
| `gateway.service.httpPort` | Gateway HTTP port | `8978` |
| `gateway.service.httpsPort` | Gateway HTTPS port (optional) | `8979` |
| `gateway.resources` | Gateway resource requests/limits | See values.yaml |
| `gateway.env` | Gateway environment variables (ConfigMap) | See values.yaml |

### UI（All-in-one 模式）

本 Chart 采用 **All-in-one** 架构：Web UI 静态资源由网关进程直接提供（同一端口 `/`），无需独立 UI Deployment/Service。

### Persistence Parameters

| Parameter | Description | Default |
|-----------|-------------|---------|
| `persistence.gatewayData.enabled` | Enable gateway data persistence | `true` |
| `persistence.gatewayData.size` | Gateway data PVC size | `10Gi` |
| `persistence.gatewayDrivers.enabled` | Enable gateway drivers persistence | `true` |
| `persistence.gatewayDrivers.size` | Gateway drivers PVC size | `1Gi` |
| `persistence.gatewayPlugins.enabled` | Enable gateway plugins persistence | `true` |
| `persistence.gatewayPlugins.size` | Gateway plugins PVC size | `1Gi` |

> PVC objects are only created when either `global.storageClass` or the per-volume `persistence.*.storageClass` is set. When no storage class is provided, the chart automatically mounts `emptyDir` volumes so the release can run on clusters that lack default storage.

### Ingress Parameters

| Parameter | Description | Default |
|-----------|-------------|---------|
| `ingress.enabled` | Enable ingress | `false` |
| `ingress.className` | Ingress class name | `""` |
| `ingress.hosts` | Ingress hosts configuration | `[]` |
| `ingress.tls` | Ingress TLS configuration | `[]` |

### Monitoring Parameters

| Parameter | Description | Default |
|-----------|-------------|---------|
| `serviceMonitor.enabled` | Enable ServiceMonitor for Prometheus | `false` |
| `podMonitor.enabled` | Enable PodMonitor for Prometheus | `false` |

### Autoscaling Parameters

| Parameter | Description | Default |
|-----------|-------------|---------|
| `gateway.autoscaling.enabled` | Enable HPA | `false` |
| `gateway.autoscaling.minReplicas` | Minimum replicas | `1` |
| `gateway.autoscaling.maxReplicas` | Maximum replicas | `10` |
| `gateway.autoscaling.targetCPUUtilizationPercentage` | Target CPU utilization | `80` |
| `gateway.autoscaling.targetMemoryUtilizationPercentage` | Target memory utilization | `80` |

## Gateway Configuration

Gateway configuration is defined in `gateway.config` in `values.yaml` using a nested structure. The configuration is automatically converted to environment variables in a ConfigMap. The gateway uses the `NG__` prefix for most configuration variables.

### Configuration Structure

The configuration uses a nested YAML structure that mirrors the gateway's internal configuration:

```yaml
gateway:
  config:
    # Logging
    logLevel: "info"           # Maps to RUST_LOG
    timezone: "Asia/Shanghai"   # Maps to TZ
    
    # General configuration
    general:
      caCertPath: "ca.crt"
      caKeyPath: "ca.key"
      
      # Southward communication
      southward:
        driverSyncStartTimeout: 2000  # milliseconds (maps to NG__GENERAL__SOUTHWARD__DRIVER_SYNC_START_TIMEOUT_MS)
      
      # Northward communication
      northward:
        queueCapacity: 10000
        appSyncStartTimeout: 2000
        cacheTtl: 3600000
      
      # Collector
      collector:
        collectionTimeout: 30000
        metricsInterval: 60000
        maxConcurrentCollections: 200
        retryAttempts: 3
        retryDelay: 1000
        outboundQueueCapacity: 10000
    
    # Web configuration
    web:
      routerPrefix: "/api"
      workers: 0
      
      ssl:
        enabled: true
        type: "auto"
        cert: "./certs/cert.pem"
        key: "./certs/key.pem"
      
      cors:
        mode: "allow_all"
      
      jwt:
        secret: "ng-gateway"
        expire: 3600000
        issuer: "ng-gateway"
    
    # Cache
    cache:
      type: "moka"
      prefix: "ng-gateway"
      delimiter: ":"
    
    # Database
    db:
      sqlite:
        path: "ng-gateway.db"
        timeout: 5000
        idleTimeout: 5000
        maxLifetime: 5000
        maxConnections: 10
        autoCreate: true
    
    # Metrics
    metrics:
      enabled: false
      endpoint: "http://localhost:4317"
      exportInterval: 60000
      serviceName: "ng"
```

### Type Conversion

All configuration values are automatically converted to strings in the ConfigMap:
- **Numbers** (integers, floats) are converted to string representation
- **Booleans** are converted to "true" or "false"
- **Strings** remain as strings

### Environment Variable Mapping

The nested configuration structure is automatically converted to environment variables:
- `logLevel` → `RUST_LOG`
- `timezone` → `TZ`
- `general.name` → `NG__GENERAL__NAME`
- `general.southward.driverSyncStartTimeout` → `NG__GENERAL__SOUTHWARD__DRIVER_SYNC_START_TIMEOUT_MS`
- `web.port` → `NG__WEB__PORT`
- `web.ssl.enabled` → `NG__WEB__SSL__ENABLED`
- And so on...

The conversion follows the pattern: nested paths are converted to uppercase with `__` separators and prefixed with `NG__` (except for special cases like `RUST_LOG` and `TZ`).

## Examples

### Basic Installation

```bash
helm install my-gateway ./ng-gateway
```

### Installation with NodePort Service

```yaml
# values.yaml
gateway:
  service:
    type: NodePort
    nodePort:
      http: 30080
      https: 30443
```

```bash
helm install my-gateway ./ng-gateway -f values.yaml
```

### Installation with Ingress

```yaml
# values.yaml
ingress:
  enabled: true
  className: "nginx"
  annotations:
    kubernetes.io/ingress.class: nginx
  hosts:
    - host: gateway.example.com
  tls:
    - secretName: gateway-tls
      hosts:
        - gateway.example.com
```

### Installation with High Availability

```yaml
# values.yaml
gateway:
  replicaCount: 3
  autoscaling:
    enabled: true
    minReplicas: 3
    maxReplicas: 10
  podDisruptionBudget:
    enabled: true
    minAvailable: 2
```

### Installation with Custom Storage

```yaml
# values.yaml
persistence:
  gatewayData:
    enabled: true
    storageClass: "fast-ssd"
    size: 50Gi
```

If you omit `storageClass`, the chart will mount `emptyDir` volumes instead of provisioning PVCs.

## Upgrading

```bash
# Upgrade with new values
helm upgrade my-gateway ./ng-gateway -f new-values.yaml

# Upgrade with set parameters
helm upgrade my-gateway ./ng-gateway \
  --set gateway.image.tag=v1.1.0
```

## Uninstallation

```bash
helm uninstall my-gateway
```

**Note:** PVCs are only created when a storage class is configured. If you created PVCs and want to keep data, set `persistence.*.enabled: false` before uninstalling, or manually delete the deployment while keeping PVCs.

## Troubleshooting

### Check Pod Status

```bash
kubectl get pods -l app.kubernetes.io/instance=my-gateway
```

### View Logs

```bash
# Gateway logs
kubectl logs -l app.kubernetes.io/component=gateway -f
```

### Check ConfigMap

```bash
kubectl get configmap my-gateway-gateway-config -o yaml
```

### Port Forward for Testing

```bash
# Gateway
kubectl port-forward svc/my-gateway-gateway 8978:8978
```

## Offline Installation

For offline installation, see the offline packaging script documentation.

## Support

For issues and questions, please refer to:
- [Project README](../../../README.md)
- [Deploy README](../../README.md)

