#!/bin/bash
set -e
echo "Building TentaFlow Desktop AppImage..."
ARCH=$(uname -m)
APP_DIR="TentaFlowAI.AppDir"
rm -rf "$APP_DIR"
mkdir -p "$APP_DIR/usr/bin"
mkdir -p "$APP_DIR/usr/share/icons/hicolor/256x256/apps"
cp ../../target/release/tentaflow-desktop "$APP_DIR/usr/bin/"
cp ../tentaflow-ai.desktop "$APP_DIR/"
echo "AppImage directory prepared at $APP_DIR"
echo "Use appimagetool to create the final AppImage"
