#!/bin/bash

# Quick Kubernetes deployment script for Prox
# This script deploys Prox to your current Kubernetes context

set -e

# Colors for output
GREEN='\033[0;32m'
BLUE='\033[0;34m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

echo -e "${BLUE}🚀 Deploying Prox to Kubernetes...${NC}"

# Check if kubectl is available
if ! command -v kubectl &> /dev/null; then
    echo "kubectl is not installed or not in PATH"
    exit 1
fi

# Check current context
CONTEXT=$(kubectl config current-context)
echo -e "${BLUE}Current context: ${YELLOW}${CONTEXT}${NC}"

# Apply the manifests
echo -e "${BLUE}📄 Applying ConfigMap...${NC}"
kubectl apply -f k8s/configmap.yaml

echo -e "${BLUE}🚀 Applying Deployment and Service...${NC}"
kubectl apply -f k8s/deployment.yaml

# Wait for deployment to be ready
echo -e "${BLUE}⏳ Waiting for deployment to be ready...${NC}"
kubectl wait --for=condition=available --timeout=300s deployment/prox

# Check rollout status
kubectl rollout status deployment/prox

# Get service information
echo -e "${GREEN}✅ Deployment successful!${NC}"
echo -e "${BLUE}📊 Service information:${NC}"
kubectl get service prox-service

echo -e "${BLUE}🎯 Pod status:${NC}"
kubectl get pods -l app=prox

# Show useful commands
echo -e "\n${GREEN}🔧 Useful commands:${NC}"
echo "  • View logs: kubectl logs -f -l app=prox"
echo "  • Port forward: kubectl port-forward svc/prox-service 8080:80"
echo "  • Scale replicas: kubectl scale deployment prox --replicas=3"
echo "  • Delete deployment: kubectl delete -f k8s/"

# If LoadBalancer, try to get external IP
EXTERNAL_IP=$(kubectl get service prox-service -o jsonpath='{.status.loadBalancer.ingress[0].ip}' 2>/dev/null || echo "")
if [[ -n "$EXTERNAL_IP" ]]; then
    echo -e "\n${GREEN}🌐 External IP: http://${EXTERNAL_IP}${NC}"
else
    echo -e "\n${YELLOW}💡 Waiting for LoadBalancer IP... Use port-forward for immediate access:${NC}"
    echo "  kubectl port-forward svc/prox-service 8080:80"
fi
