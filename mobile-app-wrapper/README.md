# TentaFlow Mobile App Wrapper

Capacitor wrapper wokół TentaFlow PWA — generuje natywny Android APK i iOS IPA
z tego samego `www/` co desktop dashboard.

## Wymagania

- **Node.js 20+** + npm/pnpm
- **Android**: Android Studio, Android SDK API 33+, JDK 17
- **iOS**: macOS + Xcode 15+, CocoaPods

## Pierwsze uruchomienie

```bash
cd mobile-app-wrapper

# Zainstaluj zależności Capacitora
npm install

# Dodaj platformy (tworzy android/ i ios/ z native projektami)
npm run add:android
npm run add:ios       # tylko na macOS

# Zsynchronizuj www/ z tentaflow-core i wygeneruj native build
npm run sync
```

## Build APK (debug)

```bash
npm run android:build
# → android/app/build/outputs/apk/debug/app-debug.apk
```

## Build APK (release, podpisany)

Wymaga keystore — wygeneruj raz:

```bash
keytool -genkey -v -keystore tentaflow.keystore -alias tentaflow \
        -keyalg RSA -keysize 2048 -validity 10000
```

Ustaw w `android/key.properties`:

```
storePassword=your-password
keyPassword=your-password
keyAlias=tentaflow
storeFile=../tentaflow.keystore
```

Potem:

```bash
npm run android:release
# → android/app/build/outputs/apk/release/app-release.apk
```

## iOS

```bash
npm run ios:open
# otwiera Xcode — Run na symulatorze / TestFlight / App Store Connect
```

## Konfiguracja serwera po stronie użytkownika

**Problem**: Natywny APK po pierwszym uruchomieniu nie wie, do którego daemona
TentaFlow się łączyć. W wersji desktop appka jest serwowana przez sam daemon
(lokalny `https://localhost:8090`), więc `window.location.origin` wystarczy.

Opcje (wybierz jedną przed pierwszym release):

### Opcja 1 — Server config screen (rekomendowana)

Do `www/js/app.js` dodaj pierwszy ekran:
```
- Wykryj czy jestesmy w Capacitor: globalThis.Capacitor?.isNativePlatform()
- Pokaz input "Adres serwera TentaFlow" (np. https://192.168.1.10:8090)
- Zapisz w localStorage jako tentaflow_server_url
- Wszystkie WS + API calls uzywaja tego URL zamiast window.location.origin
```

### Opcja 2 — mDNS discovery

Użyj `@capacitor-community/bonjour` albo własnego pluginu do wykrywania
serwerów TentaFlow w sieci lokalnej przez mDNS (ten sam `_tentaflow._tcp`
który iroh publikuje).

### Opcja 3 — QR onboarding

User na desktopie otwiera QR z adresem serwera (analogicznie do pair QR).
Phone app skanuje → zna adres → łączy.

## Deep link (tentaflow-pair://)

`capacitor.config.json` powinien dodać obsługę custom schema — patrz:
https://capacitorjs.com/docs/guides/deep-links

Android: `AndroidManifest.xml` otrzyma `<intent-filter>` dla `tentaflow-pair`
schema po `cap add android` — trzeba dodać ręcznie w
`android/app/src/main/AndroidManifest.xml`:

```xml
<intent-filter android:autoVerify="true">
    <action android:name="android.intent.action.VIEW" />
    <category android:name="android.intent.category.DEFAULT" />
    <category android:name="android.intent.category.BROWSABLE" />
    <data android:scheme="tentaflow-pair" />
</intent-filter>
```

iOS: `Info.plist` dostaje `CFBundleURLTypes` → URL scheme `tentaflow-pair`.

Po zainstalowaniu APK, skanowanie QR aparatem systemowym otwiera appkę
automatycznie.

## Dystrybucja

### Android
- **Sideload** przez bezpośredni APK — `adb install tentaflow.apk`
- **Google Play** — trzeba konto developera ($25 jednorazowo) + 2FA + opisy
- **Alt stores**: F-Droid, APKPure — łatwiej ale mniej userów

### iOS
- **TestFlight** — tylko z konta Apple Developer ($99/rok), max 100 testerów
  external / 10,000 internal. Wygodne, niepubliczne.
- **App Store** — ten sam koszt, pełna review (1-2 tyg na start).
- **Sideload** — tylko z Xcode na własne urządzenie z Apple ID (7 dni).

## Status

**Na teraz:** scaffolding gotowy, `npm install && npm run add:android` dział.
Build APK da się zrobić po zainstalowaniu Android SDK.

**Co brakuje do production APK:**
1. Server config screen (Opcja 1/2/3 wyżej)
2. Ikony i splash screen w wielu rozdzielczościach
   (`@capacitor/assets` generuje z jednego 1024×1024 source)
3. Signing config
4. Privacy manifest (iOS 17.5+ wymaga)
5. Play Store / App Store metadata (opisy, screenshoty)
