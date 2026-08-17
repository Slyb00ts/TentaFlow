#!/usr/bin/env bash
# ===== File: run.sh — build and run the Apple experiments =====
# Usage: ./run.sh        -> EKS-A1 + EKS-A3
#        ./run.sh a2     -> EKS-A2
set -euo pipefail
cd "$(dirname "$0")"
case "${1:-a1a3}" in
  a2)   swiftc -O -framework Metal -framework Foundation eks_a2.swift -o eks_a2 && ./eks_a2 ;;
  *)    swiftc -O -framework Metal -framework Foundation eks_apple.swift -o eks_apple && ./eks_apple ;;
esac
