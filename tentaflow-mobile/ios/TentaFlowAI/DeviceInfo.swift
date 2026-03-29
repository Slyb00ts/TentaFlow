// =============================================================================
// Plik: DeviceInfo.swift
// Opis: FFI helpers — informacje o urzadzeniu iOS (nazwa, RAM) dla Rust core
// =============================================================================

import UIKit

/// Zwraca nazwe urzadzenia (np. "Piotrek's iPhone") — Rust zwalnia przez free()
@_cdecl("tentaflow_get_device_name")
func tentaflowGetDeviceName() -> UnsafeMutablePointer<CChar>? {
    let name = UIDevice.current.name
    return strdup(name)
}

/// Zwraca ilosc RAM w MB
@_cdecl("tentaflow_get_ram_mb")
func tentaflowGetRamMb() -> UInt64 {
    return ProcessInfo.processInfo.physicalMemory / (1024 * 1024)
}
