# Prometheus & Grafana Monitoring Stack for Prox

This comprehensive guide explains how to set up and configure both Prometheus and Grafana to monitor your Prox reverse proxy with a complete production-ready monitoring stack.

## Prerequisites

- Prox built and running with metrics enabled
- Docker and Docker Compose installed
- Basic understanding of monitoring concepts
- Familiarity with YAML configuration files

## Architecture Overview

The monitoring stack consists of three main components:

```
┌─────────────┐    ┌──────────────┐    ┌─────────────┐
│    Prox     │───▶│  Prometheus  │───▶│   Grafana   │
│ (Port 3000) │    │ (Port 9090)  │    │(Port 3001)  │
│             │    │              │    │             │
│/metrics     │    │Scrapes every │    │Visualizes   │
│endpoint     │    │15 seconds    │    │metrics      │
└─────────────┘    └──────────────┘    └─────────────┘
```

**Flow:**
1. **Prox** exposes metrics at `https://localhost:3000/metrics`
2. **Prometheus** scrapes these metrics every 15 seconds
3. **Grafana** queries Prometheus to visualize the data

## Complete Stack Setup

### Step 1: Configuration Files

The monitoring stack requires these files at your project root:

**docker-compose.yml** (Container orchestration)
**prometheus.yml** (Prometheus configuration)

### Step 2: Docker Compose Configuration

Your `docker-compose.yml` should contain:

```yaml
services:
  prometheus:
    image: prom/prometheus:latest
    container_name: prox-prometheus
    ports:
      - "9090:9090"
    volumes:
      - ./prometheus.yml:/etc/prometheus/prometheus.yml:ro
    command:
      - '--config.file=/etc/prometheus/prometheus.yml'
      - '--storage.tsdb.path=/prometheus'
      - '--web.console.libraries=/etc/prometheus/console_libraries'
      - '--web.console.templates=/etc/prometheus/consoles'
      - '--storage.tsdb.retention.time=200h'
      - '--web.enable-lifecycle'
    restart: unless-stopped

  grafana:
    image: grafana/grafana:latest
    container_name: prox-grafana
    ports:
      - "3001:3000"
    environment:
      - GF_SECURITY_ADMIN_PASSWORD=admin
      - GF_USERS_ALLOW_SIGN_UP=false
      - GF_INSTALL_PLUGINS=grafana-piechart-panel
      # Auto-configure Prometheus datasource
      - GF_DATASOURCES_DEFAULT_NAME=Prometheus
      - GF_DATASOURCES_DEFAULT_TYPE=prometheus
      - GF_DATASOURCES_DEFAULT_URL=http://prometheus:9090
      - GF_DATASOURCES_DEFAULT_ACCESS=proxy
      - GF_DATASOURCES_DEFAULT_IS_DEFAULT=true
    volumes:
      - grafana-storage:/var/lib/grafana
    restart: unless-stopped

volumes:
  grafana-storage:
```

### Step 3: Prometheus Configuration

Your `prometheus.yml` should contain:

```yaml
global:
  scrape_interval: 15s
  evaluation_interval: 15s

rule_files:
  # - "first_rules.yml"
  # - "second_rules.yml"

scrape_configs:
  - job_name: 'prometheus'
    static_configs:
      - targets: ['localhost:9090']

  - job_name: 'prox'
    scheme: 'https'
    tls_config:
      insecure_skip_verify: true
    static_configs:
      - targets: ['host.docker.internal:3000']
    metrics_path: '/metrics'
    scrape_interval: 15s
```

**Key Configuration Notes:**
- **scheme: 'https'** - Prox runs on HTTPS
- **insecure_skip_verify: true** - Accepts self-signed certificates
- **host.docker.internal:3000** - Docker networking to reach host
- **scrape_interval: 15s** - Collects metrics every 15 seconds

## Quick Start Guide

### 1. Start the Complete Stack

```bash
# Start Prometheus and Grafana
docker-compose up -d

# Start Prox (in another terminal)
cargo run --release

# Verify everything is working
./test-monitoring-stack.sh
```

### 2. Access the Services

- **Prox Server:** https://localhost:3000
- **Prometheus:** http://localhost:9090  
- **Grafana:** http://localhost:3001 (admin/admin)

### 3. Quick Verification

```bash
# Check if Prox metrics are available
curl -k https://localhost:3000/metrics

# Check Prometheus targets
curl http://localhost:9090/api/v1/targets

# Generate test traffic
for i in {1..10}; do curl -k https://localhost:3000/health; done
```

## Prometheus Deep Dive

### Understanding Prometheus Configuration

**Global Settings:**
```yaml
global:
  scrape_interval: 15s     # How often to scrape targets
  evaluation_interval: 15s  # How often to evaluate rules
```

**Scrape Targets:**
```yaml
scrape_configs:
  - job_name: 'prox'
    scheme: 'https'                    # Prox uses HTTPS
    tls_config:
      insecure_skip_verify: true       # Accept self-signed certs
    static_configs:
      - targets: ['host.docker.internal:3000']  # Docker networking
    metrics_path: '/metrics'           # Metrics endpoint
    scrape_interval: 15s              # Override global interval
```

### Prometheus Web Interface

**Useful Prometheus URLs:**
- **Main Interface:** http://localhost:9090
- **Targets Status:** http://localhost:9090/targets
- **Configuration:** http://localhost:9090/config
- **Service Discovery:** http://localhost:9090/service-discovery

**Testing Queries in Prometheus:**
1. Go to http://localhost:9090/graph
2. Try these basic queries:
   ```promql
   prox_requests_total
   rate(prox_requests_total[1m])
   up{job="prox"}
   ```

### Prometheus Troubleshooting

**Common Issues:**

1. **Target Down:**
   ```
   Problem: Prox target shows as "DOWN" in Prometheus
   Solution: Check if Prox is running and /metrics is accessible
   ```

2. **No Metrics:**
   ```
   Problem: Prometheus scraping but no prox_ metrics
   Solution: Verify Prox metrics are enabled and endpoint works
   ```

3. **SSL/TLS Errors:**
   ```
   Problem: Certificate verification failed
   Solution: Ensure insecure_skip_verify: true in prometheus.yml
   ```

**Debug Commands:**
```bash
# Check Prometheus logs
docker logs prox-prometheus

# Test metrics endpoint manually
curl -k https://localhost:3000/metrics | grep prox_

# Validate Prometheus config
docker exec prox-prometheus promtool check config /etc/prometheus/prometheus.yml
```

## Grafana Setup & Configuration

### Initial Grafana Setup

**First Login:**
1. Open http://localhost:3001 in your browser
2. Login with `admin/admin`
3. Grafana will prompt you to change the password (optional for development)

**Verify Prometheus Datasource:**
The datasource is automatically configured via Docker Compose environment variables, but you can verify:

1. Click on **Configuration** (⚙️) → **Data sources**
2. You should see **Prometheus** listed and working
3. Click **Test** to verify the connection
4. URL should be: `http://prometheus:9090`

### Understanding Grafana Interface

**Main Navigation:**
- **Home:** Dashboard overview
- **Dashboards:** Manage and create dashboards  
- **Explore:** Ad-hoc query interface
- **Alerting:** Set up alerts and notifications
- **Configuration:** Datasources, users, preferences

### Creating Your First Dashboard

**Step 1: Create a New Dashboard**

1. Click **+ (Plus)** → **Dashboard**
2. Click **Add new panel**
3. You'll see the query editor at the bottom

**Step 2: Basic Metrics Panel**

**Panel Title:** Request Rate
**Query:** `rate(prox_requests_total[1m])`

1. In the query field, enter: `rate(prox_requests_total[1m])`
2. Set **Panel title** to "Request Rate (req/sec)"
3. Under **Panel options** → **Unit** → **Throughput** → **requests/sec**
4. Click **Apply**

**Step 3: Add More Panels**

Click **Add panel** to add additional monitoring panels.

## Essential Prox Monitoring Panels

### 1. Request Rate
```promql
rate(prox_requests_total[1m])
```
**Purpose:** Shows requests per second
**Panel Type:** Time series
**Unit:** requests/sec

### 2. Request Duration (Latency)
```promql
histogram_quantile(0.95, rate(prox_request_duration_seconds_bucket[5m]))
```
**Purpose:** 95th percentile response time
**Panel Type:** Time series
**Unit:** seconds

### 3. Active Connections
```promql
prox_active_connections
```
**Purpose:** Current number of active connections
**Panel Type:** Stat
**Unit:** short

### 4. Request Status Codes
```promql
rate(prox_requests_total[1m])
```
**Purpose:** Request rates by status code
**Panel Type:** Time series
**Legend:** `{{status}} - {{method}}`

### 5. Backend Health Status
```promql
prox_backend_health_status
```
**Purpose:** Health status of backend servers
**Panel Type:** Stat
**Thresholds:** 0 = Red, 1 = Green

### 6. Error Rate
```promql
rate(prox_requests_total{status=~"4..|5.."}[1m])
```
**Purpose:** Rate of 4xx and 5xx errors
**Panel Type:** Time series
**Unit:** requests/sec

### 7. Request Duration Heatmap
```promql
rate(prox_request_duration_seconds_bucket[1m])
```
**Purpose:** Distribution of request durations
**Panel Type:** Heatmap
**Unit:** seconds

### 8. Top Endpoints
```promql
topk(10, rate(prox_requests_total[1m]))
```
**Purpose:** Most active endpoints
**Panel Type:** Table
**Legend:** `{{endpoint}}`

## Advanced Dashboard Configuration

### Dashboard Settings

1. Click **Dashboard settings** (⚙️ icon)
2. **General**:
   - Title: "Prox Reverse Proxy Monitoring"
   - Description: "Real-time metrics for Prox performance and health"
   - Tags: `prox`, `reverse-proxy`, `monitoring`

3. **Time options**:
   - Auto-refresh: `5s` or `10s`
   - Time range: `Last 1 hour`

### Panel Customization

#### Time Series Panels
- **Display**: Lines, points, or both
- **Axis**: Log scale for wide value ranges
- **Legend**: Show/hide, position (bottom, right)
- **Tooltip**: All series, single series, or none

#### Stat Panels
- **Value options**: Last, min, max, mean
- **Text size**: Auto, or custom size
- **Color scheme**: Based on thresholds

#### Thresholds and Alerts
Set up visual indicators:
- **Green**: Good performance (< 100ms response time)
- **Yellow**: Warning (100-500ms response time)  
- **Red**: Critical (> 500ms response time)

## Sample Dashboard Layout

```
┌─────────────────┬─────────────────┬─────────────────┐
│   Request Rate  │   Error Rate    │ Active Conns    │
│   (Time Series) │  (Time Series)  │    (Stat)       │
├─────────────────┼─────────────────┼─────────────────┤
│        Request Duration (P95)     │ Backend Health  │
│           (Time Series)           │    (Stat)       │
├───────────────────────────────────┼─────────────────┤
│         Status Codes              │  Top Endpoints  │
│        (Time Series)              │    (Table)      │
├───────────────────────────────────┴─────────────────┤
│            Request Duration Heatmap                 │
│                 (Heatmap)                           │
└─────────────────────────────────────────────────────┘
```

## Useful PromQL Queries for Prox

### Performance Monitoring
```promql
# Request rate by endpoint
sum(rate(prox_requests_total[1m])) by (endpoint)

# Average response time
rate(prox_request_duration_seconds_sum[1m]) / rate(prox_request_duration_seconds_count[1m])

# Request rate by status code
sum(rate(prox_requests_total[1m])) by (status)

# 99th percentile latency
histogram_quantile(0.99, rate(prox_request_duration_seconds_bucket[5m]))
```

### Health Monitoring
```promql
# Backend availability
avg(prox_backend_health_status) by (backend)

# Connection utilization
prox_active_connections / prox_max_connections * 100

# Error rate percentage
sum(rate(prox_requests_total{status=~"4..|5.."}[1m])) / sum(rate(prox_requests_total[1m])) * 100
```

### Capacity Planning
```promql
# Peak request rate in last 24h
max_over_time(rate(prox_requests_total[1m])[24h])

# Connection usage trend
increase(prox_active_connections[1h])

# Response time trend
avg_over_time(histogram_quantile(0.95, rate(prox_request_duration_seconds_bucket[5m]))[24h])
```

## Dashboard Export/Import

### Exporting Dashboard
1. Click **Dashboard settings** → **JSON Model**
2. Copy the JSON
3. Save to file: `prox-dashboard.json`

### Importing Dashboard
1. Click **+ (Plus)** → **Import**
2. Paste JSON or upload file
3. Configure datasource if needed
4. Click **Import**

## Alerting Setup

### Creating Alerts

1. Edit a panel → **Alert** tab
2. **Create Alert**
3. Set conditions:
   ```
   WHEN avg() OF query(A, 1m, now) IS ABOVE 0.5
   ```
4. Set evaluation frequency: `10s`
5. Configure notification channels

### Sample Alert Rules

**High Error Rate:**
```
sum(rate(prox_requests_total{status=~"5.."}[1m])) > 0.1
```

**High Response Time:**
```
histogram_quantile(0.95, rate(prox_request_duration_seconds_bucket[5m])) > 1
```

**Backend Down:**
```
prox_backend_health_status < 1
```

## Variables and Templating

### Adding Variables

1. **Dashboard settings** → **Variables** → **Add variable**
2. **Name:** `backend`
3. **Type:** Query
4. **Query:** `label_values(prox_requests_total, backend)`
5. **Multi-value:** Enable
6. **Include All:** Enable

### Using Variables in Queries
```promql
rate(prox_requests_total{backend=~"$backend"}[1m])
```

## Troubleshooting Guide

### Prometheus Issues

**Problem: Prox target showing as DOWN**
```bash
# Check if Prox is running and accessible
curl -k https://localhost:3000/metrics

# Check Prometheus logs
docker logs prox-prometheus

# Verify network connectivity from Prometheus container
docker exec prox-prometheus wget -qO- --no-check-certificate https://host.docker.internal:3000/metrics
```

**Problem: No metrics being scraped**
```bash
# Verify Prometheus configuration
curl http://localhost:9090/api/v1/targets

# Check scrape errors
docker logs prox-prometheus | grep -i error

# Test manual scrape
curl -X POST http://localhost:9090/-/reload
```

**Problem: High memory usage**
```bash
# Check Prometheus storage
docker exec prox-prometheus du -sh /prometheus

# Monitor memory usage
docker stats prox-prometheus

# Reduce retention time in docker-compose.yml
# Add: '--storage.tsdb.retention.time=72h'
```

### Grafana Issues

**Problem: Datasource connection fails**
```bash
# Test Prometheus from Grafana container
docker exec prox-grafana wget -qO- http://prometheus:9090/api/v1/query?query=up

# Check Grafana logs
docker logs prox-grafana

# Verify environment variables
docker exec prox-grafana env | grep GF_
```

**Problem: No data in dashboards**
```bash
# Check time range (set to "Last 5 minutes")
# Verify query syntax in Prometheus first
curl "http://localhost:9090/api/v1/query?query=prox_requests_total"

# Check if data exists
curl "http://localhost:9090/api/v1/label/__name__/values" | jq '.data[]' | grep prox
```

**Problem: Dashboard panels show "N/A"**
```bash
# Generate test traffic
for i in {1..20}; do curl -k https://localhost:3000/health; sleep 0.5; done

# Check query results directly
curl "http://localhost:9090/api/v1/query?query=rate(prox_requests_total[1m])"

# Verify labels match your queries
curl -k https://localhost:3000/metrics | grep prox_requests_total
```

### Network Issues

**Problem: Docker containers can't reach host**
```bash
# On macOS/Windows: host.docker.internal should work
# On Linux: use --network host or find host IP
docker exec prox-prometheus nslookup host.docker.internal

# Alternative: Use host network mode
# Add to docker-compose.yml: network_mode: "host"
```

**Problem: Port conflicts**
```bash
# Check what's using ports
lsof -i :9090  # Prometheus
lsof -i :3001  # Grafana
lsof -i :3000  # Prox

# Change ports in docker-compose.yml if needed
```

### Common PromQL Errors

**Problem: "No data points" error**
```promql
# Wrong: prox_requests_total (raw counter)
# Right: rate(prox_requests_total[1m]) (rate of change)

# Wrong: rate(prox_requests_total[1s]) (too short interval)
# Right: rate(prox_requests_total[1m]) (reasonable interval)
```

**Problem: "Unknown metric" error**
```bash
# List all available metrics
curl -k https://localhost:3000/metrics | grep "^prox_"

# Check exact metric names
curl http://localhost:9090/api/v1/label/__name__/values | jq -r '.data[]' | grep prox
```

## Performance Tuning

### Prometheus Optimization

**Storage Configuration:**
```yaml
# In docker-compose.yml, add to prometheus command:
- '--storage.tsdb.retention.time=168h'      # 1 week retention
- '--storage.tsdb.retention.size=10GB'      # Size-based retention
- '--storage.tsdb.wal-compression'          # Enable WAL compression
```

**Query Performance:**
```yaml
# Add query limits
- '--query.timeout=30s'
- '--query.max-concurrency=20'
- '--query.max-samples=50000000'
```

### Grafana Optimization

**Query Optimization:**
- Use longer time intervals for historical data
- Limit the number of series returned
- Use recording rules for complex queries

**Dashboard Performance:**
```json
{
  "refresh": "10s",
  "time": {
    "from": "now-1h",
    "to": "now"
  },
  "timepicker": {
    "refresh_intervals": ["5s", "10s", "30s", "1m", "5m"]
  }
}
```

## Advanced Features

### Recording Rules

Create `rules.yml` for complex queries:
```yaml
groups:
  - name: prox_rules
    rules:
      - record: prox:request_rate
        expr: rate(prox_requests_total[1m])
        
      - record: prox:error_rate
        expr: rate(prox_requests_total{status=~"4..|5.."}[1m])
        
      - record: prox:p95_latency
        expr: histogram_quantile(0.95, rate(prox_request_duration_seconds_bucket[5m]))
```

### Alerting Rules

Add to Prometheus configuration:
```yaml
rule_files:
  - "alerts.yml"

alerting:
  alertmanagers:
    - static_configs:
        - targets:
          - alertmanager:9093
```

Create `alerts.yml`:
```yaml
groups:
  - name: prox_alerts
    rules:
      - alert: HighErrorRate
        expr: prox:error_rate > 0.1
        for: 2m
        labels:
          severity: warning
        annotations:
          summary: "High error rate detected"
          
      - alert: HighLatency
        expr: prox:p95_latency > 1
        for: 5m
        labels:
          severity: critical
        annotations:
          summary: "High latency detected"
```

## Next Steps & Best Practices

### Monitoring Strategy

1. **Start Simple:** Begin with basic metrics (requests, latency, errors)
2. **Add Gradually:** Introduce more complex metrics as needed
3. **Monitor the Monitors:** Keep an eye on Prometheus/Grafana resource usage
4. **Document Everything:** Keep your dashboards and alerts well-documented

### Operational Excellence

1. **Regular Backups:** Automate backup of dashboards and configurations
2. **Capacity Planning:** Monitor storage growth and plan accordingly
3. **Security Updates:** Keep Prometheus and Grafana updated
4. **Access Control:** Implement proper authentication and authorization

### Integration Opportunities

1. **Log Aggregation:** Combine with ELK stack or Loki
2. **APM Integration:** Connect with Jaeger for distributed tracing  
3. **Cloud Monitoring:** Integrate with cloud provider monitoring
4. **Business Metrics:** Add custom business-specific metrics to Prox

## Production Considerations

### Performance Optimization

**Prometheus:**
- **Retention:** Default 200h, adjust based on disk space
- **Storage:** Monitor disk usage in `/prometheus` path
- **Memory:** ~1GB RAM per million samples ingested per day
- **Network:** Scraping adds network overhead

**Grafana:**
- **Query Performance:** Use appropriate time ranges and intervals
- **Dashboard Limits:** Keep panels under 20 per dashboard
- **Cache:** Enable query result caching for better performance
- **Plugins:** Only install necessary plugins

### Security Hardening

**Prometheus Security:**
```yaml
# Add to prometheus.yml for production
global:
  external_labels:
    environment: 'production'
    
# Enable basic auth (requires additional setup)
# web:
#   basic_auth_users:
#     admin: $2b$12$hNf2lSsxfm0.i4a.1kVpSOVyBOlln6kXkNFW2kJWxzjuVJa4PxHIq
```

**Grafana Security:**
```yaml
# Add to docker-compose.yml for production
environment:
  - GF_SECURITY_ADMIN_PASSWORD=${GRAFANA_PASSWORD:-admin}
  - GF_USERS_ALLOW_SIGN_UP=false
  - GF_AUTH_ANONYMOUS_ENABLED=false
  - GF_SERVER_ROOT_URL=https://your-domain.com/grafana
  - GF_SECURITY_COOKIE_SECURE=true
```

### Scalability & High Availability

**Prometheus HA Setup:**
- Run multiple Prometheus instances
- Use external storage (e.g., Thanos, Cortex)
- Implement federation for large-scale deployments

**Grafana HA Setup:**
- Use external database (PostgreSQL, MySQL)
- Shared storage for dashboards
- Load balancer for multiple instances

### Backup & Recovery

**Prometheus Data:**
```bash
# Backup Prometheus data
docker run --rm -v prox_prometheus-data:/data alpine tar czf - /data > prometheus-backup.tar.gz

# Restore Prometheus data
cat prometheus-backup.tar.gz | docker run --rm -i -v prox_prometheus-data:/data alpine tar xzf - -C /
```

**Grafana Configuration:**
```bash
# Export all dashboards
curl -X GET http://admin:admin@localhost:3001/api/search | \
  jq -r '.[] | select(.type == "dash-db") | .uid' | \
  xargs -I {} curl -X GET http://admin:admin@localhost:3001/api/dashboards/uid/{} > dashboards-backup.json

# Backup Grafana volume
docker run --rm -v prox_grafana-storage:/data alpine tar czf - /data > grafana-backup.tar.gz
```

## Integration with CI/CD

### Automated Testing

**Include monitoring in your CI pipeline:**
```yaml
# .github/workflows/monitoring-test.yml
name: Test Monitoring Stack
on: [push, pull_request]

jobs:
  monitoring-test:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v2
      
      - name: Start monitoring stack
        run: docker-compose up -d
        
      - name: Build and start Prox
        run: |
          cargo build --release
          ./target/release/prox --config config.yaml &
          sleep 10
          
      - name: Test monitoring stack
        run: ./test-monitoring-stack.sh
        
      - name: Cleanup
        run: docker-compose down
```

### Dashboard as Code

**Version control your dashboards:**
```bash
# Export dashboard JSON
curl -X GET http://admin:admin@localhost:3001/api/dashboards/uid/prox-main > dashboards/prox-main.json

# Import via API
curl -X POST http://admin:admin@localhost:3001/api/dashboards/db \
  -H "Content-Type: application/json" \
  -d @dashboards/prox-main.json
```

## References

- [Grafana Documentation](https://grafana.com/docs/)
- [Prometheus Query Language](https://prometheus.io/docs/prometheus/latest/querying/basics/)
- [Grafana Panel Types](https://grafana.com/docs/grafana/latest/panels/)
- [PromQL Functions](https://prometheus.io/docs/prometheus/latest/querying/functions/)

---

**Pro Tip:** Use the test script `./test-monitoring-stack.sh` to generate traffic and see your dashboards come alive with real data!

## Monitoring Stack Management

### Starting and Stopping Services

**Start the complete stack:**
```bash
# Start Prometheus and Grafana
docker-compose up -d

# Check container status
docker-compose ps

# View logs
docker-compose logs -f prometheus
docker-compose logs -f grafana
```

**Stop the stack:**
```bash
# Stop all services
docker-compose down

# Stop and remove volumes (WARNING: deletes Grafana data)
docker-compose down -v
```

**Restart individual services:**
```bash
# Restart Prometheus (reload config)
docker-compose restart prometheus

# Restart Grafana
docker-compose restart grafana
```

### Configuration Management

**Updating Prometheus Configuration:**
1. Edit `prometheus.yml`
2. Restart Prometheus: `docker-compose restart prometheus`
3. Verify: http://localhost:9090/config

**Prometheus Configuration Validation:**
```bash
# Validate config before applying
docker run --rm -v $(pwd)/prometheus.yml:/prometheus.yml \
  prom/prometheus:latest promtool check config /prometheus.yml
```

**Grafana Data Persistence:**
- Dashboards and settings are stored in Docker volume `grafana-storage`
- To backup: `docker run --rm -v prox_grafana-storage:/data alpine tar czf - /data`
- Data survives container restarts but not `docker-compose down -v`

### Health Monitoring

**Quick Health Check:**
```bash
# Check all services
curl http://localhost:9090/-/healthy  # Prometheus
curl http://localhost:3001/api/health # Grafana
curl -k https://localhost:3000/health # Prox

# Use the comprehensive test script
./test-monitoring-stack.sh
```

**Service Status Commands:**
```bash
# Docker container status
docker ps | grep prox-

# Resource usage
docker stats prox-prometheus prox-grafana

# Service logs
docker logs --tail 50 prox-prometheus
docker logs --tail 50 prox-grafana
```
