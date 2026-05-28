# Spike: RocksDB Build Matrix

## Podsumowanie

Minimalny crate testowy w `/tmp/tentaflow-rocksdb-spike` potwierdzil, ze crate
`rocksdb = 0.24.0` buduje wbudowany `librocksdb-sys = 0.17.3+10.4.2` i dziala
na host Linux. Cross-build Android rowniez przechodzi dla `aarch64` i `x86_64`,
ale wymaga jawnego ustawienia NDK `CC`, `CXX`, `AR` i linkera.

iOS, macOS i Windows nie zostaly w pelni potwierdzone na tej maszynie, bo
srodowisko Linux nie ma Xcode `xcrun`, macOS runnera ani Windows/MSVC lub MinGW
toolchainu. To nie jest blad RocksDB, tylko brak docelowych toolchainow w tej
sesji.

## Testowany kod

Minimalny test:

```rust
pub fn roundtrip(path: &std::path::Path) -> Result<Vec<u8>, rocksdb::Error> {
    let db = rocksdb::DB::open_default(path)?;
    db.put(b"sync-ledger-key", b"sync-ledger-value")?;
    db.flush()?;
    let value = db.get(b"sync-ledger-key")?;
    Ok(value.unwrap_or_default())
}
```

## Wyniki

| Platforma | Target | Status | Wynik |
|-----------|--------|--------|-------|
| Linux host | `x86_64-unknown-linux-gnu` | OK | `cargo test` przeszedl, zapis/odczyt RocksDB dziala |
| Android | `aarch64-linux-android` | OK po konfiguracji NDK | `cargo check` przeszedl |
| Android | `x86_64-linux-android` | OK po konfiguracji NDK | `cargo check` przeszedl |
| iOS | `aarch64-apple-ios` | Zablokowane przez srodowisko | brak `xcrun` na Linux |
| macOS | `aarch64-apple-darwin` / `x86_64-apple-darwin` | Niezweryfikowane lokalnie | wymaga runnera macOS |
| Windows | MSVC albo GNU | Niezweryfikowane lokalnie | brak targetu i toolchainu Windows na tej maszynie |

## Komendy, ktore przeszly

Linux:

```bash
cargo test
```

Android ARM64:

```bash
env -u RUSTC_WRAPPER \
  CARGO_HOME=/tmp/tentaflow-cargo-home \
  CC_aarch64_linux_android=/opt/android-ndk/toolchains/llvm/prebuilt/linux-x86_64/bin/aarch64-linux-android35-clang \
  CXX_aarch64_linux_android=/opt/android-ndk/toolchains/llvm/prebuilt/linux-x86_64/bin/aarch64-linux-android35-clang++ \
  AR_aarch64_linux_android=/opt/android-ndk/toolchains/llvm/prebuilt/linux-x86_64/bin/llvm-ar \
  CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER=/opt/android-ndk/toolchains/llvm/prebuilt/linux-x86_64/bin/aarch64-linux-android35-clang \
  cargo check --target aarch64-linux-android
```

Android x86_64:

```bash
env -u RUSTC_WRAPPER \
  CARGO_HOME=/tmp/tentaflow-cargo-home \
  CC_x86_64_linux_android=/opt/android-ndk/toolchains/llvm/prebuilt/linux-x86_64/bin/x86_64-linux-android35-clang \
  CXX_x86_64_linux_android=/opt/android-ndk/toolchains/llvm/prebuilt/linux-x86_64/bin/x86_64-linux-android35-clang++ \
  AR_x86_64_linux_android=/opt/android-ndk/toolchains/llvm/prebuilt/linux-x86_64/bin/llvm-ar \
  CARGO_TARGET_X86_64_LINUX_ANDROID_LINKER=/opt/android-ndk/toolchains/llvm/prebuilt/linux-x86_64/bin/x86_64-linux-android35-clang \
  cargo check --target x86_64-linux-android
```

## Problemy wykryte

1. `librocksdb-sys` domyslnie szuka `aarch64-linux-android-clang++`, ale NDK
   instaluje narzedzia z API levelem w nazwie, np.
   `aarch64-linux-android35-clang++`.
2. Globalny `~/.cargo/config.toml` ustawia `rustc-wrapper = "/usr/bin/sccache"`.
   W sandboxie powodowalo to `sccache: Operation not permitted`. Test przeszedl
   dopiero po uruchomieniu z tymczasowym `CARGO_HOME` bez wrappera.
3. Cross-build iOS z Linuxa zatrzymuje sie na `cc-rs: failed to find tool
   "xcrun"`. Do potwierdzenia iOS potrzebny jest realny macOS/Xcode runner.

## Wniosek

RocksDB jest nadal akceptowalnym kierunkiem dla Sync Ledger, ale przed
dopisywaniem zaleznosci do `tentaflow-core` trzeba dodac stale build-checki:

- Linux: `cargo test` albo `cargo check`
- Android ARM64: `cargo check --target aarch64-linux-android` z NDK env
- Android x86_64: `cargo check --target x86_64-linux-android` z NDK env
- macOS: osobny runner macOS
- iOS: osobny runner macOS z Xcode i `xcrun`
- Windows: osobny runner Windows, preferowany MSVC

## Nastepne zadanie

Nastepny krok z planu to zadanie 2: audyt `mesh/crdt.rs`, `mesh/crdt_store.rs`,
obecnego CRDT sync i miejsc zapisu addonow.
