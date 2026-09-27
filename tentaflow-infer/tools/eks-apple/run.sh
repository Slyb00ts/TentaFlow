#!/usr/bin/env bash
# ===== File: run.sh — build and run the Apple experiments =====
# Usage: ./run.sh        -> EKS-A1 + EKS-A3
#        ./run.sh a2     -> EKS-A2
#        ./run.sh a9 <katalog_modeli> [sekcja] -> EKS-A9 (modele: eks_a9_gen.py)
#        ./run.sh a10 <katalog_modeli> [sekcja] -> EKS-A10 faza 0 (modele: eks_a9_gen.py --a10)
#        ./run.sh a10multi <katalog_modeli> [sweep|sweep-inproc|run] [klucz=wartość …] -> EKS-A10 wiele modeli naraz
set -euo pipefail
cd "$(dirname "$0")"
case "${1:-a1a3}" in
  a2)   swiftc -O -framework Metal -framework Foundation eks_a2.swift -o eks_a2 && ./eks_a2 ;;
  a9)   swiftc -O -framework CoreML -framework Metal -framework Accelerate -framework CoreVideo \
          -framework IOSurface -framework Foundation eks_a9_ane.swift -o eks_a9_ane \
        && ./eks_a9_ane "${2:?katalog z modelami .mlmodelc}" "${3:-all}" ;;
  a10)  swiftc -O -framework CoreML -framework Accelerate -framework Foundation eks_a10_ane.swift -o eks_a10_ane \
        && ./eks_a10_ane "${2:?katalog z modelami .mlmodelc}" "${3:-all}" ;;
  a10multi) swiftc -O -framework CoreML -framework Foundation eks_a10_multi.swift -o eks_a10_multi \
        && ./eks_a10_multi "${2:?katalog z modelami .mlmodelc}" "${3:-sweep}" "${@:4}" ;;
  *)    swiftc -O -framework Metal -framework Foundation eks_apple.swift -o eks_apple && ./eks_apple ;;
esac
