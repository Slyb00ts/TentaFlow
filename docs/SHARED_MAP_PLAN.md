# Wspólna mapa lokalizacji (Shared Map) — plan wdrożenia

Status: PLAN v2 (po recenzji). Zastępuje obecny model „jedna scena na robota, mapa = ostatnia klatka" (`services/slam_scene.rs`, `tentaflow-slam/src/voxel_map.rs`) modelem: **lokalizacja (site) → mapa (scene) → trwała siatka zajętości (occupancy) budowana przez wszystkie urządzenia**. Nadrzędne dokumenty: `docs/UNIFIED_SLAM_ARCHITECTURE.md` (§1, §6, §13, §15), `docs/SPATIAL_3D_PLAN.md` (§10.2, §11, §12). Reguły z `CLAUDE.md` obowiązują w całości (binarny CBOR `MessageBody`, dashboard bez REST, brak shimów, komponenty `tf-*`, i18n w 5 lokalizacjach, brak TODO). Odpowiedzi na recenzję niezależną i uzasadnienie odrzuconych uwag: §14.

## Decyzje użytkownika (2026-09-22) — wiążą nad treścią poniżej

1. **Każde urządzenie buduje własną mapę; mapy nakładające się na te same miejsca łączą się automatycznie.** Mechanizm „prowizorycznej submapy" (§2.4 pkt 4) staje się zasadą ogólną, a nie wyjątkiem: każda sesja urządzenia zaczyna we własnej submapie; w tle działa dopasowanie submapa↔mapa lokalizacji i submapa↔submapa (BnB + ICP, §2.4), a po przejściu bramki (unikalność + drugie potwierdzenie) submapy są scalane transformacją. Scalenie nigdy nie jest cofane po cichu — błędne scalenie operator może rozdzielić w UI (P3).
2. **Telefony**: przechwytywanie istnieje (`tentaflow-mobile/ios/TentaFlowAI/SensorBridge.swift`, `android/.../MainActivity.kt:147-175` + `DepthCapture.kt`), ale startuje automatycznie przy uruchomieniu aplikacji, bez przełącznika i bez widocznego stanu. Plan dostaje zadanie w P2: w aplikacji mobilnej przełącznik „Udostępniaj skanowanie do mapy" + stan (ARKit/ARCore dostępne?, klatki/s, przypisana lokalizacja), a w dashboardzie telefon widoczny na liście urządzeń lokalizacji.
3. **Właściciel mapy (węzeł)**: wybierany przy tworzeniu lokalizacji — domyślnie węzeł, na którym admin ją tworzy, z możliwością wyboru innego węzła z listy (preferowane węzły stacjonarne z GPU; telefony nie mogą być właścicielem). Widoczny w ustawieniach lokalizacji; zmiana ręczna przez admina (§6.5). Bez automatycznego failoveru.
4. **Tożsamość robota**: `addon_id` instancji jest już unikalny per instancja (np. `go2-30c979b5`), więc dwa Go2 = dwie instancje addonu = dwa urządzenia. Trwałym identyfikatorem urządzenia w mapie jest **numer seryjny** (pobierany z konta Unitree, `aes_key`/`serial` w konfiguracji instancji); adres IP się zmienia (DHCP — robot przeszedł z 192.168.0.190 na 192.168.50.250), więc nie może być tożsamością. `map_device_placements.device_id` = serial, gdy znany, w przeciwnym razie `addon_id` instancji. Pytanie 5 zamknięte.
5. **Go2 bez Ethernetu** (robot mobilny, tylko Wi-Fi) — patrz pytanie 1 (rozstrzygnięte).

## Stan wykonania (2026-09-23)

**P0 zrobione** (backend + UI lokalizacji/map): migracje 165 (`map_sites`, `map_scenes`,
`map_device_placements`, `map_chunks`, `map_device_sessions`) i 166 (`map.read`/`map.write`/
`map.admin`), `MapPayload` w trzech miejscach (`tentaflow-protocol/src/map.rs`,
`tentaflow-protocol-wasm`, `www/js/protocol/codec.js`), `dispatch/maps.rs` z bramką RBAC,
`paths::maps_dir()`, deskryptory sync + materializer + capture (site/scene/placement),
`www/js/modules/maps.js` z i18n w pięciu lokalizacjach. Testy: `dispatch::maps` (RBAC, izolacja
organizacji, epoka właściciela, wymuszone usunięcie sceny z geometrią), `migration_v165`,
`registry_contains_the_map_tables_but_not_the_geometry`, roundtrip protokołu, `maps.test.js`.

Dwie świadome różnice wobec treści poniżej:

1. **Nie ma kolumn `deleted_at_ms`.** W całym `tentaflow-core` nie istnieje soft delete — usunięcie
   replikuje się jako operacja `ActionType::Delete`, a LWW rozstrzyga ją przez `is_lww_tracked` +
   `core_resource_versions`. Dodanie tu tombstone'ów wymyśliłoby drugi mechanizm usuwania dla
   jednej funkcji.
2. **`SiteListResponse` niesie `my_permissions`.** Dashboard nie odtwarza mapowania rola →
   uprawnienia (tabela `roles` jest edytowalna przez admina, więc kopia po stronie klienta
   rozjechałaby się i pokazywała przyciski, które serwer odrzuca).

**P1 w toku** (`tentaflow-slam`): `occupancy.rs` (log-odds, chunki 128³/bloki 16³ SoA, bramka
trwałości 3 trafienia w ≥1 s, usunięcie dopiero przy progu pewności ORAZ dwóch punktach obserwacji
≥0,3 m od siebie, scena przy limicie odmawia zamiast eksmitować), `carve.rs` (DDA zatrzymywane na
pierwszej powierzchni klatki, window-diff ograniczony do części wspólnej okien, wykluczenia działające
i na punkty, i na promień) oraz `chunk_io.rs` (`.tfmc`, WAL, kompakcja, kwarantanna uszkodzonego
snapshotu). Zostaje `reloc.rs` (BnB + ICP) i benchmarki, których progi są zablokowane pomiarem P0.5.

Nie zrobione w P0 i nieudawane: widok 3D (`tf-map-view`), strumień `map:`, umieszczanie i
relokalizacja urządzeń, `SceneClearRegionRequest`, przejęcie właściciela od nieosiągalnego węzła
(`accept_loss` jedzie jako `false`), migracja danych `GEO_ANCHORS_SETTING` wraz z usunięciem
`RobotsPayload::GeoAnchor*` i sekcji w `robots.js` — to jedna zmiana z usunięciem starego kodu,
więc idzie razem z P2.

## 0. Cel i definicje

- **Site (lokalizacja)**: obiekt fizyczny (budynek, hala, teren). Zarządza nim admin. Wiele lokalizacji na organizację.
- **Scene (mapa)**: jedna spójna rekonstrukcja w JEDNYM układzie metrycznym (Z-up, prawoskrętny, metry). Należy do dokładnie jednej lokalizacji. Domyślnie 1 mapa na lokalizację; model dopuszcza wiele (np. piętra) — patrz pytanie 6.
- **Device (urządzenie)**: `(node_id, device_id)`; `device_id` to identyfikator robota, pod którym węzeł go ogłasza (`AdvertisedRobot.robot_id`, dziś == `addon_id`: `go2`, `phone`). Kolumna jest nieprzezroczystym tekstem: platforma dziś nie rozróżnia dwóch instancji tego samego addonu robota na jednym węźle (`LidarStreamHub`, `SlamSceneManager`, telemetria — wszystko kluczowane `addon_id`), więc dwa Go2 na jednym węźle są nierozróżnialne **na poziomie platformy, nie tylko tego planu**. Plan nie rozwiązuje tego sam; wymaga decyzji (pytanie 5 [BLOKUJĄCE]). Telefon jest własnym węzłem mesh (`services/mobile_sensors.rs`: „ONE device per node"), więc klucz `phone` jest unikalny w połączeniu z `node_id`.
- **Device session (sesja)**: okres, w którym układ odometrii urządzenia jest ciągły. Restart addonu / robota / sesji AR = nowa sesja = nowe `placement`.
- **Placement**: `T_device_odom→scene` (SE(3), f64) dla danej sesji urządzenia. Jedyna transformacja, która wprowadza klatki urządzenia do sceny. **Jedynym pisarzem placementów jest węzeł-właściciel sceny** (§2.5, §6.1) — to usuwa ryzyko dwóch źródeł prawdy (ręczne vs automatyczne).
- **Occupancy voxel**: komórka 0,05 m (domyślnie; per scena) z log-odds, licznikiem trafień i rewizją ostatniej obserwacji.

Zasada nadrzędna: **mapa jest własnością serwera (węzeł-właściciel sceny), jest trwała na dysku, replikowana przez mesh (metadane przez Sync Ledger, geometria przez pull), a przeglądarka jest wyłącznie widokiem** (żadnego unionowania po stronie klienta, jak dziś w `robots.js:534-740`).

## 1. Model danych

### 1.1 SQLite (metadane; migracja 165 `shared_maps`, 166 `shared_maps_permissions`)

Tabele (wszystkie z `org_id`, `created_at_ms`, `updated_at_ms`, `deleted_at_ms NULL` — soft delete jest konieczny dla LWW w Sync Ledger):

```
map_sites            site_id PK, org_id, name, description, address, lat, lon, alt,
                     created_by, created_at_ms, updated_at_ms, deleted_at_ms
map_scenes           scene_id PK, site_id FK, org_id, name, voxel_res_m REAL (default 0.05),
                     owner_node_id TEXT, owner_epoch INTEGER, geo_lat, geo_lon, geo_alt, geo_heading
                     (NULL = brak georeferencji), max_voxels INTEGER, tfmc_version INTEGER,
                     created_by, *_at_ms
map_device_placements scene_id, node_id, device_id, session_epoch INTEGER,
                     tx,ty,tz,qx,qy,qz,qw REAL (f64), method TEXT ('identity'|'manual'|'icp'),
                     locked INTEGER (1 = korekta automatyczna nie zapisuje; §2.5),
                     confidence REAL, set_by, set_at_ms, updated_at_ms, deleted_at_ms
                     PK(scene_id,node_id,device_id,session_epoch)
map_chunks           scene_id, chunk_key TEXT ('cx_cy_cz'), revision INTEGER, owner_epoch INTEGER,
                     sha256 TEXT, size_bytes INTEGER, occupied INTEGER, updated_hlc TEXT,
                     PK(scene_id,chunk_key)                       -- INDEKS geometrii, nie jej nośnik (§6.2)
map_device_sessions  node_id, device_id, session_epoch, scene_id, started_at_ms, last_frame_ms,
                     state TEXT ('unplaced'|'relocalizing'|'placed'|'lost'),
                     clock_offset_us INTEGER, clock_offset_samples INTEGER   -- LOKALNA, nie synchronizowana (§2.7)
```

Nie ma w v1 tabel `map_anchors` ani `map_change_events`: kotwice fizyczne czekają na detekcję tagów (pytanie 8), a zdarzenia zmian w v1 są wyłącznie sygnałem w strumieniu delt (§5); tabela, klastrowanie i potwierdzanie przechodzą do fazy P7. `method` zawiera tylko trzy wartości, które da się dziś wyprodukować.

Uprawnienia w `roles`: `map.read`, `map.write`, `map.admin` (§9). Migracja seeduje przez `roles_add_permissions` (wzorzec `roles_add_robot_permissions`, `db/migrations.rs:6263-6268`).

Usuwamy klucz ustawień `GEO_ANCHORS_SETTING` (`dispatch/robots.rs:390-430`) — georeferencja jest własnością sceny (`map_scenes.geo_*`), nie robota. **Migracja danych**: 165 czyta istniejące wpisy `GEO_ANCHORS_SETTING`; dla każdego robota z kotwicą tworzy site `Zmigrowane: <robot_id>` + scenę z `geo_*` z kotwicy i placement `identity` dla `(local_node, robot_id)`, po czym usuwa klucz. Nic nie ginie po cichu; admin może scalić/usunąć takie sceny w UI. Jeśli klucz jest pusty, migracja nic nie tworzy (pytanie 11).

### 1.2 Geometria na dysku (NIE w SQLite, NIE w Fjall, NIE w `core.blob` — zgodnie z SPATIAL §10.2 pkt 2)

Katalog: `paths::maps_dir()` = `data_dir()/maps/<scene_id>/` (nowa funkcja w `paths.rs`, obok `blobs_dir`/`recordings_dir`).

```
maps/<scene_id>/
  chunks/<cx>_<cy>_<cz>.<revision>.tfmc     -- niezmienne snapshoty chunków (mmap do odczytu)
  pending/<node>_<device>_<epoch>.tfmc      -- prowizoryczna submapa sesji bez placementu (§2.4 pkt 4)
  wal.log                                   -- append-only dziennik zmian od ostatnich snapshotów (tylko właściciel)
  scene.meta                                -- magic, wersja formatu, voxel_res, wymiary bloków, ostatnia rewizja, rola (owner|replica), owner_epoch
```

Hierarchia przestrzenna: voxel 0,05 m → **blok** 16³ voxeli (0,8 m) → **chunk** 8³ bloków (6,4 m). Chunk = jednostka trwałości, replikacji, streamowania i LOD.

Format `.tfmc` (LE): `magic "TFMC"`, `version u16`, `scene_id u64`, `chunk_key i32×3`, `voxel_res f32`, `revision u64`, `owner_epoch u32`, `cell_count u32`, `payload_len u32`, payload **zstd** SoA: `cell_idx u32[]` (128³ = 2 097 152 komórek w chunku), `log_odds i8[]`, `hits u8[]` (nasycone), `last_seen_rev u32[]`, `first_seen_delta u16[]` (odległość rewizji od pierwszej obserwacji, nasycona — próg trwałości potrzebuje ≈ 1 s, nie pełnej historii), `flags u8[]` (bit0 stable, bit1 dynamic-suspect, bit2 removed-pending); stopka `crc32c` (Castagnoli — to jest zależność workspace; `crc32fast` nie jest i nie dokładamy nowej). 13 B/komórkę przed kompresją (`cell_idx` 4 + `log_odds` 1 + `hits` 1 + `last_seen_rev` 4 + `first_seen_delta` 2 + `flags` 1; wcześniejsze „9 B" w tym planie było błędem arytmetycznym). Zapis: tylko komórki, które kiedykolwiek miały trafienie (przestrzeń wolna/nieznana nie jest materializowana — §3.1). Liczniki LOD **nie są** przechowywane — liczone przy publikacji snapshotu do strumienia (§7.2).

Polityka wersji `.tfmc`: czytnik akceptuje wyłącznie bieżącą `version`. Podbicie wersji = jednorazowa przepisanie wszystkich chunków sceny przez właściciela przy pierwszym załadowaniu (`scene.meta` i `map_scenes.tfmc_version` podbite po sukcesie; do tego czasu ingest wstrzymany), replika po zobaczeniu nowej `tfmc_version` w indeksie kasuje własne pliki i pobiera ponownie. Brak czytników wielu wersji (CLAUDE.md: brak shimów).

Uszkodzony snapshot (zły `magic`/`crc32`/`payload_len`): plik przenoszony do `chunks/corrupt/`, chunk oznaczony jako brakujący. Właściciel: odtwarza z WAL, jeśli WAL zawiera pełny zakres rewizji od poprzedniego snapshotu; w przeciwnym razie pobiera chunk o `sha256` z indeksu od dowolnej repliki (`MapChunkPull`, §6.2) i dopiero na nim odtwarza WAL. Replika: pobiera od właściciela. Gdy nikt nie ma poprawnej kopii — chunk pusty, alert w UI (`chunk_lost`), ingest kontynuowany.

WAL: własny nagłówek pliku `magic "TFMW" + version u16` (czytnik przyjmuje dokładnie jedną wersję;
zły magic/wersja to twardy błąd otwarcia sceny, nie kwarantanna — log jest wspólny dla sceny, więc
odłożenie go na bok wyrzuciłoby wszystkie klatki od ostatniej kompakcji), rekord
`{revision u64, chunk_key, n u16, [cell_idx u32, log_odds i8, hits u8, last_seen_rev u32, first_seen_delta u16, flags u8]×n}` + crc32c — rekord niesie PEŁNY stan komórki, bo odtwarzanie `last_seen_rev`/`first_seen_delta` z samego `hits`/`log_odds` przestaje rozróżniać trafienie od chybienia przy nasyceniu obu liczników; flush co 250 ms, `fsync` co 1 s. Kompakcja chunku: gdy bajty WAL dla chunku > rozmiar jego snapshotu albo chunk „brudny" ≥ 30 s → nowy snapshot `<rev>.tfmc`, stary usuwany po zapisie i po aktualizacji wiersza `map_chunks`; `wal.log` obcinany, gdy wszystkie brudne chunki mają snapshot. Start: otwórz snapshoty (mmap) → odtwórz `wal.log` → gotowe. Testy odtwarzania po awarii w §11.

Budżet: liczby poniżej są **oszacowaniem z parametrów formatu, nie pomiarem**; pomiar na fixture z prawdziwego Go2 jest bramką P0.5. Przy 5 cm: biuro 1000 m² ≈ 2–3 mln zajętych voxeli → RAM ~9 B/voxel + narzut haszy ≈ 30–40 MB; dysk zstd ≈ 2–3 B/voxel ≈ 8 MB. Hala 10 000 m² ≈ 25–30 mln voxeli ≈ 300 MB RAM / 80 MB dysku. `map_scenes.max_voxels` domyślnie 50 mln; po przekroczeniu ingest odrzuca nowe komórki i podnosi alert w UI (nie po cichu jak dziś `DEFAULT_SCENE_VOXEL_CAP` z FIFO). Limit repliki na telefonie — pytanie 14.

## 2. Rejestracja i lokalizacja urządzeń w scenie

### 2.1 Przypisanie do lokalizacji
Urządzenie musi być przypisane do sceny (`map_device_placements` z `method='identity'` lub innym) zanim jego klatki trafią do mapy. Nieprzypisane urządzenie: tylko podgląd live (`lidar:`), a w UI karta robota pokazuje akcję „Przypisz do mapy". Bez automatycznego tworzenia scen. Pierwsze uruchomienie bez żadnej lokalizacji: użytkownik bez `map.admin` widzi w `#/maps` pusty stan „Brak lokalizacji — poproś administratora" (nazwiska adminów org z `directory`), bez przycisku tworzenia; admin widzi kreator lokalizacji + mapy w jednym oknie.

### 2.2 Sesje i epoki
Nie ma w `AddonManager` haka „instancja wystartowała". Są haki **zatrzymania**: `SlamSceneManager::remove` przy uninstall (`addon/mod.rs:1615`) i przy zatrzymaniu ostatniej instancji (`addon/mod.rs:3025`); oba stają się `SharedMapManager::device_stopped(device_id)`, który zamyka sesję (`state='lost'`, WAL zrzucony). Nową sesję otwiera **pierwsza klatka lub poza po zamknięciu** albo, w trakcie sesji: (a) `frame_seq` spadł, (b) pozycja skoczyła o > 1,0 m między kolejnymi próbkami pozy odległymi < 200 ms (Go2 resetuje odometrię przy starcie, ARKit przy nowej sesji), (c) `odom_epoch` w próbce pozy (§2.7) wzrósł. Nowa sesja ⇒ `session_epoch += 1`, stan `unplaced` → relokalizacja (§2.4). Do czasu `placed` klatki i pozy trafiają do bufora sesji: pierścień ograniczony **15 s lub 8 MB** (Go2 ~5 Hz × kilka tysięcy voxeli — kilkaset KB; telefon RawDepth po downsamplingu 0,1 m); przekroczenie = drop-oldest z licznikiem.

### 2.3 Pierwsze urządzenie w pustej scenie
Placement = identity, `method='identity'` (gauge sceny — jak `gauge_anchor` w `graph.rs:208`). Operator może potem obrócić/przesunąć całą scenę względem planu (operacja „ustaw północ / origin" — modyfikuje `geo_*`, nie geometrię).

### 2.4 Relokalizacja (nowa sesja w znanej scenie) — kolejność
1. **Scan-to-map** (`tentaflow-slam/src/reloc.rs`, nowy): chmura z bufora sesji (Go2: suma okien z ostatnich 3 s po deduplikacji komórek; telefon: chmura depth po downsamplingu 0,1 m) vs stabilna mapa sceny. Etap A: **wielorozdzielczościowe przeszukanie korelacyjne z branch-and-bound** (Hess et al. 2016, jak w Cartographerze): piramida siatek max-pool 0,2/0,4/0,8/1,6 m liczona z LOD mapy (§7.2) przy starcie relokalizacji, DFS po (x, y, yaw) z górnym ograniczeniem wyniku z grubszego poziomu, z-offset z histogramu wysokości podłogi; promień 30 m od ostatniej znanej pozy, w przeciwnym razie cała scena. Naiwne 650 M porównań bez BnB nie mieści się w celu; z BnB typowo odwiedzanych jest < 1 % liści. Cel: ≤ 5 s na jednym rdzeniu, mierzone w benchmarku P1 na fixture. Etap B: doprecyzowanie **point-to-plane ICP** istniejącym `lidar::icp::register` (`lidar/icp.rs:61`) na LOD-0. Bramka (§5 UNIFIED): inlier ratio ≥ 0,6, RMS ≤ 0,1 m, **drugie niezależne potwierdzenie**: klatka ≥ 1 s później musi dać wynik w 0,2 m / 3°; kandydat drugi w rankingu musi być gorszy o ≥ 30 % (unikalność). Pass ⇒ `method='icp'`, `confidence` z `cov_from_icp` (`service.rs:245`); fail ⇒ stan `relocalizing`, widoczny w UI, z możliwością ręcznego dopasowania.
2. **Ręcznie**: UI umieszcza urządzenie na mapie (§8.3); `method='manual'`, `locked=1`; po zatwierdzeniu uruchamiany jest etap 1B (ICP) jako doprecyzowanie z limitem 0,5 m / 15° — poza limitem wynik odrzucony, zostaje ręczny.
3. **Ponawianie**: w stanie `relocalizing` etap 1 powtarza się co 5 s na rosnącej submapie (pkt 4) — nowy widok może usunąć niejednoznaczność korytarza.
4. **Robot poza znaną mapą** (start w nieskanowanej części budynku): zamiast czekać w nieskończoność, sesja buduje **prowizoryczną submapę** w układzie własnej odometrii (własny `OccupancyGrid` w RAM, limit 2 mln voxeli, trwałość w `pending/<sesja>.tfmc` przy zatrzymaniu/kompakcji), niewidoczną w `map:` poza warstwą „niedopasowane" dla operatora. Gdy relokalizacja (pkt 1 lub 2) się powiedzie — submapa jest przekształcona przez placement i **scalona** ze sceną (trafienia dodawane z `hits` zachowanymi, `first_seen_delta` liczone od scalenia) i plik `pending/` usuwany. Gdy sesja kończy się bez placementu, submapa zostaje w `pending/` 7 dni, potem jest usuwana (operator może ją dopasować ręcznie w tym czasie). Domyślna polityka; alternatywa „czekaj na operatora" — pytanie 2 [BLOKUJĄCE].

Kotwice fizyczne (AprilTag/QR) i dok nie są w tej fazie: brak detekcji tagów w kamerach (pytanie 8).

### 2.5 Korekta dryfu w trakcie sesji — jeden pisarz, blokada ręcznego placementu
Go2 (opcja B): ufamy odometrii lokalnie, ale co 3 m przebytej drogi lub 10 s wykonujemy ICP okna vs mapa stabilna (tylko voxele `stable`, bez `dynamic-suspect`). Korekta jest akceptowana, gdy ≤ 0,1 m / 2° na krok; większa → flaga `drift_warning` w telemetrii i brak korekty (nigdy skok mapy). Telefon (ARKit/ARCore ma własne loop-closure) — ten sam mechanizm, próg 0,2 m.

Zapis korekty: **wyłącznie właściciel sceny** pisze `map_device_placements` (żądania `DevicePlaceRequest` z innych węzłów są przekazywane do właściciela komendą mesh, §6.1) — nie ma dwóch pisarzy jednego wiersza, więc LWW po HLC nigdy nie rozstrzyga między ręcznym a automatycznym. Dla `locked=1` (każdy placement `manual`) korekta działa **tylko w pamięci** i jest raportowana jako `drift_estimate` (pose7 + wielkość) w `DeviceListResponse`; DB nie jest dotykana. Operator może „Przyjmij korektę" (`DevicePlaceRequest{method:'icp', locked:0}` z bieżącym estymatem) albo „Odblokuj". Dla `locked=0` korekta jest zapisywana co 60 s.

Graf póz / submapy z `tentaflow-slam` (graph.rs, submap.rs, optimize.rs, loop_closure.rs) NIE są używane w tej fazie dla źródeł pre-fused (Go2, ARKit). Pozostają dla źródeł surowych (LIO, faza późniejsza) — nie kasujemy ich, mają testy i są częścią planu §12 UNIFIED.

### 2.6 Telefony
- iOS: `SensorBridge.swift:182,227` daje pozę i punkty w Z-up. Android: `DepthCapture.kt:89-151` również (niedokończone jest tylko dopasowanie AR↔ENU w `localization.rs`, które nie jest potrzebne do mapy lokalnej).
- Dziś `feed_pose` (`host_functions/sensors.rs:87-93`) idzie do `LocalizationEngine::ingest_pose`, a to `localization.rs:91,187` woła `SlamSceneManager::on_pose`; `feed_depth` (`sensors.rs:99-105`) woła `SlamSceneManager::on_lidar_frame`. Po zmianie: `LocalizationEngine` zostaje (ESKF/geo), ale jego dwa wywołania `on_pose` kierują do `SharedMapManager::on_device_pose(local_node, device_id, …)`, a `feed_depth` do `on_device_frame(local_node, device_id, …)`.
- Klatki telefonu powstają na węźle telefonu, a scena żyje na węźle-właścicielu → **strumień ingestu przez mesh** (§6.3). `SharedMapManager` na węźle telefonu, gdy właściciel sceny jest zdalny, pompuje klatki i pozy do odwróconego bi-streamu.
- Klucz urządzenia: `(node_id, "phone")`; nazwa wyświetlana = `sync_nodes.display_name`.
- **Autoryzacja ingestu telefonu**: transport wymaga zaufanego węzła (istniejąca polityka mesh); prawo do pisania w scenie daje **wiersz przypisania** utworzony przez użytkownika z `map.write` (z dowolnego UI, także na telefonie, jeśli zalogowany użytkownik ma `map.write`). Węzeł bez wiersza przypisania dostaje `PolicyDenied` na pierwszej klatce. Alternatywa „autoryzacja per węzeł przez admina" — pytanie 3 [BLOKUJĄCE].
- Replika chunków na telefonie: domyślnie **nie** (tylko strumień `map:`), limit rozmiaru — pytanie 14.

### 2.7 Zegary i poza z addonu (nowa funkcja hosta)
Stan dzisiejszy, zmierzony w kodzie: poza Go2 dociera do core **tylko co 10 s** — `SlamSceneManager::on_pose` jest wołany z `mesh/robot_dispatch.rs:467-474` wewnątrz `refresh_local_advertisement`, napędzanego interwałem 10 s (`mesh/pipeline.rs:3337`), ze stemplem `now_us()` chwili odczytu, a addon (`go2/src/lib.rs:1573-1596`) zapisuje pozę do telemetrii bez znacznika czasu. Każda reguła „klatka bez pozy w ±300 ms → drop" odrzuciłaby przy tym niemal wszystkie klatki. Dlatego:

- **Nowa funkcja hosta** `robot.pose_publish_v1` (addon-sdk, scope `lidar.publish`): `PoseSampleV1 { timestamp_us i64, odom_epoch u32, pos [f64;3], quat [f64;4], velocity Option<[f64;3]> }`. Addon Go2 woła ją z `ingest_robot_pose` na każdą wiadomość `rt/utlidar/robot_pose` (ten sam zegar co nagłówek klatki LiDAR: WASI realtime, `go2/src/lib.rs:242-244, 1414`), `odom_epoch` rośnie przy wykrytym resecie odometrii. Wywołanie `on_pose` w `robot_dispatch.rs:467-474` jest **usuwane**; `telemetry.pose_position/orientation` zostają dla karty robota.
- **Domeny zegarów** (wszystkie wall-clock, ale różne maszyny): (1) host core — addon Go2 działa w wasmtime na hoście core, więc jego `timestamp_us`, `depth_mapping::depth_timestamp_us` (`depth_mapping.rs:627`) i `DetectionsMessage.ts_ms` (czas przechwycenia klatki, `detection_bus.rs:211`; ms → µs) są **jednym zegarem**; (2) każdy węzeł telefonu — `PoseSample.timestamp_us` i nagłówek klatki depth pochodzą z tego samego zegara telefonu.
- **Reguła**: dopasowanie pozy do klatki (`pose_at`) odbywa się zawsze **w domenie zegara źródła** (poza i klatka tego samego urządzenia), bez przeliczeń. Przesunięcie zegara jest potrzebne tylko między urządzeniami (maski „znane pozy urządzeń", §4 pkt 2; detekcje kamery robota A dla klatek telefonu B) i tam tolerancja ±500 ms jest wystarczająca dzięki marginesom wolumenów.
- **Estymacja przesunięcia per urządzenie zdalne** (`map_device_sessions.clock_offset_us`): dla każdej próbki ze strumienia ingestu `offset = arrival_us(owner) − timestamp_us(source)`; utrzymywane minimum z okna 64 próbek (filtr minimum jak w NTP — najmniejsze opóźnienie transportu najlepiej przybliża prawdziwe przesunięcie), potem EMA 0,1. Urządzenia lokalne: offset = 0.
- **Klatki z przyszłości**: po korekcie offsetu `timestamp_us > arrival_us + 200 ms` ⇒ klatka odrzucana (`future_frame` w telemetrii); ≥ 10 kolejnych ⇒ offset resetowany (skok zegara źródła).

## 3. Model zajętości i aktualizacji

### 3.1 Struktura (`tentaflow-slam/src/occupancy.rs`, nowy; zastępuje `voxel_map.rs`)
`OccupancyGrid { res, chunks: HashMap<ChunkKey, Chunk> }`, `Chunk { blocks: HashMap<u16 /*block idx*/, Block>, revision, dirty }`, `Block` SoA 4096 komórek: `log_odds: [i8;4096]` (0 = nieznane), `hits: [u8;4096]` (nasycone), `last_seen_rev: [u32;4096]`, `first_seen_delta: [u16;4096]`, `flags: [u8;4096]`. Skala log-odds: i8 = wartość × 32 (zakres ±3,97). Parametry (OctoMap): `l_hit=+0,85`, `l_miss=-0,4`, `l_min=-2,0`, `l_max=+3,5`, próg `occupied ≥ +0,85`, próg `free ≤ -0,4`.

Komórka jest **widoczna** (`stable`), gdy `log_odds ≥ +0,85` **i** `hits ≥ 3` (trafienia liczone tylko z klatek odległych ≥ 200 ms) **i** `first_seen_delta` odpowiada ≥ 1,0 s (rewizje mapowane na czas przez znaną kadencję klatek sceny).
Komórka **znika** (usuwana z widoku, flaga `removed-pending` → usunięcie fizyczne po kompakcji), gdy `log_odds ≤ -0,4` po ≥ 3 obserwacjach wolnej przestrzeni z ≥ 2 różnych pozycji sensora (odległość ≥ 0,3 m) — chroni przed jednorazowym błędnym promieniem. Czy znikanie ma wymagać wielu potwierdzeń w dłuższym horyzoncie (dni) — pytanie 12.

### 3.2 Aktualizacja per klatka (`tentaflow-slam/src/carve.rs`)
Wejście: `DeviceFrame { device, session_epoch, timestamp_us, points_frame: Odom | Sensor, origin_frame [f32;3], points, kind: PrefusedWindow | RawDepth, mask: Option<ExclusionSet> }`.
1. Poza z czasu klatki: `pose_at(timestamp_us)` (dziś `scene_pose_at`, `slam_scene.rs:326`) — naprawia błąd użycia `last_pose` w `fold_frame:257`. Wymóg pozy zależy od `points_frame`:
   - `Odom` (Go2 `voxel_map_compressed` — punkty już w układzie odometrii robota): poza NIE jest potrzebna do umieszczenia punktów (tylko `placement`); służy wyłącznie jako środek okna do carvingu i marker urządzenia. Brak pozy w ±1 s ⇒ klatka **wchodzi** (trafienia), bez carvingu; licznik `frames_without_pose`.
   - `Sensor` (telefon, kamera depth — punkty w układzie sensora): brak pozy w ±300 ms ⇒ klatka odrzucona (licznik w telemetrii). Telefony publikują pozy z ARKit/ARCore z częstością ≥ 30 Hz, więc reguła jest realna.
2. Transformacja `placement ∘ pose_at` (lub samo `placement` dla `Odom`) → punkty w scenie (f32 lokalnie w chunku, f64 dla póz).
3. **Trafienia**: każdy punkt → komórka, `log_odds += l_hit` (raz na komórkę na klatkę), `hits += 1` (jeśli ≥ 200 ms od poprzedniego trafienia), `last_seen_rev = rev`.
4. **Carving** (obserwacje wolnej przestrzeni), dwie strategie:
   - `PrefusedWindow` (Go2 `voxel_map_compressed`, promień okna ~5 m): **window-diff** — komórki mapy leżące w oknie poprzedniej i bieżącej klatki tego urządzenia (przecięcie kul o promieniu okna), obecne poprzednio w mapie stabilnej, a nieobecne w bieżącej klatce, dostają `l_miss`. Strategia zakłada, że Go2 sam wycina wolną przestrzeń w swoim oknie (wtedy jest dokładna i tania, O(komórki w oknie)). **To założenie jest sprawdzane na fixture w P0.5** (postaw pudło, zabierz, policz klatki do zniknięcia z okna Go2); jeśli okno Go2 nie usuwa zajętości, `PrefusedWindow` używa DDA jak `RawDepth` z promieniem okna. Decyzja zapisana w raporcie P0.5, nie w tym pliku.
   - `RawDepth` (kamera z modelem głębi, telefon): **ray-cast DDA** (Amanatides–Woo) od `origin` do każdego punktu; tylko istniejące komórki mapy na promieniu dostają `l_miss`; przejście zatrzymuje się na pierwszej komórce trafionej w bieżącej klatce. Promienie kamer monokularowych: `l_miss` × 0,5 (mniejsza wiarygodność), zasięg ≤ `MAX_DEPTH_M`.
5. Maski wykluczeń (§4) stosowane PRZED krokiem 3 (punkty w masce są odrzucane) i w kroku 4 (komórki w masce nie dostają `l_miss`, żeby przechodzący człowiek nie „wycinał" ściany za sobą przez błędy głębi).
6. Zwrot `FrameDelta { chunk_key → (added_stable: Vec<u32>, removed: Vec<u32>) }` do WAL i strumienia.

Koszt CPU — **budżet ustalany z pomiaru**, nie z założeń: Go2 daje „kilka tysięcy zajętych voxeli na klatkę" przy ~5 Hz (`go2/src/lib.rs:836-838`), kamera depth 190k/3 ≈ 63k punktów. Bramka P1 (wpisana po P0.5 na podstawie fixture): ≤ 40 ms/klatkę depth i ≤ 10 ms/klatkę Go2 na jednym rdzeniu Grace; z `rayon` per-chunk (partycjonowanie promieni po chunku docelowym, dwie fazy: zbierz → zastosuj) ≤ 1/4 tego. `rayon` jest w workspace (`Cargo.toml:226`), ale **nie** w `tentaflow-slam` — dochodzi `rayon = { workspace = true }` w jego manifeście. **GPU nie jest wymagane w v1.** Rezerwa: kernel DDA jako compute shader `wgpu` (wgpu jest już zależnością core) albo CUDA przez Burn — uruchamiany dopiero, gdy benchmark P1 nie mieści się w budżecie przy 4 równoległych źródłach RawDepth. Model głębi kamer już działa na GPU (`depth_mapping.rs`, Burn/wgpu lub CUDA) — pozostaje bez zmian.

## 4. Wykluczanie obiektów dynamicznych

Trzy niezależne warstwy, każda z osobna wystarczająca do „przechodzący człowiek nie zostaje w mapie":

1. **Filtr trwałości** (§3.1): `hits ≥ 3` w ≥ 1 s. Człowiek idący 1 m/s zostawia w komórce 1–2 trafienia. Działa dla wszystkich sensorów bez detekcji.
2. **Znane pozy urządzeń**: dla każdego `placed` urządzenia w scenie wolumen ciała (Go2: 0,80 × 0,40 × 0,50 m + 0,15 m marginesu; telefon: kula 0,5 m wokół pozy) jest `ExclusionSet` dla klatek innych urządzeń i własnych (Go2 nie widzi siebie, ale telefon skanujący z bliska widzi robota). Poza obcego urządzenia jest brana z czasu klatki po korekcie `clock_offset_us` (§2.7), tolerancja ±500 ms.
3. **Detekcje z kamer** (interfejs: `detection_bus::subscribe(camera_id)` → `Detection { klasa, bbox, score, track_id, vx, vy }` + `DetectionsMessage.ts_ms` = wall-clock ms przechwycenia klatki (`detection_bus.rs:211`), **nie PTS strumienia** — do dopasowania z klatkami LiDAR przeliczany na µs w domenie hosta; plan detekcji GPU osób/obiektów — `docs/ROBOT_CAMERA_PRIVACY_PLAN.md` — dostarcza klasy; kontrakt tutaj: klasy dynamiczne = konfigurowalna lista, domyślnie `person, animal, dog, cat, robot, vehicle, bicycle`; `score ≥ 0,4`; `track_id` ≠ 0 lub `|vx|+|vy| > 0,02`). Dla kamery przypisanej do robota (`DepthMappingConfig { camera_id, robot_id, fov_deg, fov_v_deg, pitch_deg }`, `db/repository.rs:23880`) bbox → **frustum** w układzie kamery (bbox rozszerzony o 10 %, głębokość: jeśli dostępna mapa głębi z `depth_mapping` — mediana głębi w bboxie ± 0,6 m; inaczej [0,3 m, `MAX_DEPTH_M`×2]) → do sceny przez `placement ∘ pose_at(ts_detekcji)`. Dopasowanie czasu: detekcja ważna dla klatek LiDAR/depth w oknie ±150 ms; przy braku detekcji w oknie (kamera wolniejsza) używana jest ostatnia detekcja tego `track_id` ekstrapolowana przez `vx, vy` do 500 ms. Komórki wewnątrz frustum: brak trafień, flaga `dynamic-suspect` z szybszym zanikiem (`l_miss × 2` przy kolejnych obserwacjach wolnej przestrzeni).
   Kamery telefonów: identycznie, jeśli detekcje dla `camera_id` telefonu będą publikowane na jego węźle (przesyłane w strumieniu ingestu, §6.3) — pytanie 9.

Kalibracja kamera↔ciało robota: wykorzystujemy istniejące `pitch_deg` + nowe `mount_yaw_deg`, `mount_offset_m` w `DepthMappingConfig` (rozszerzenie istniejącej struktury, nie nowa).

## 5. Detekcja zmian (pojawiło się / zniknęło)

v1: **sygnał w strumieniu**, bez tabeli. `FrameDelta` (§3.2 pkt 6) niesie przejścia `unknown/free → stable` (appeared) i `stable → removed` (disappeared); ramka `Delta` w `map:` (§7.2) przekazuje je z `state` i `source_mask`, a renderer koloruje je czasowo (§7.3). Operator widzi „co się zmieniło w ostatnich N minutach" jako warstwę wizualną; nie ma listy, klastrowania, potwierdzeń ani retencji.

P7 (po P6): `shared_map/changes.rs` — okno 5 s, klastrowanie 26-sąsiedztwem na LOD-1, próg klastra, tabela `map_change_events` z bbox/dowodami/ack, protokół `ChangeEvents*`, UI tabeli. Retencja i wymóg wielokrotnych potwierdzeń — pytanie 12.

## 6. Synchronizacja mesh

### 6.1 Metadane
`map_sites, map_scenes, map_device_placements, map_chunks` dodane do `CORE_SYNC_DESCRIPTORS` (`sync/core_registry.rs`), `scope: Organization`, `retention: Durable`, jeden `partition_suffix: "maps"` — `map_chunks` jest małym indeksem (jeden wiersz na chunk, aktualizowany co kompakcję ≥ 30 s), więc osobna partycja nie jest potrzebna. Materializacja: `core_materializer.rs` — LWW po HLC; `map_chunks` i `map_scenes` dodatkowo odrzucają wiersz z `owner_epoch` < lokalnie znanego dla sceny. `ensure_default_core_sync_policies` obejmie je automatycznie (iteruje po deskryptorach).

Mutacje sceny wykonuje właściciel: `DevicePlaceRequest`, `DeviceAssignRequest`, `DeviceRelocalizeRequest`, `SceneClearRegionRequest` odebrane na węźle innym niż `owner_node_id` są przekazywane komendą `MeshCommandType::MapOwnerCommand { scene_id, payload: Vec<u8> /*CBOR MapPayload*/ }` (request/response, krótkie) i wykonywane tam po tej samej kontroli RBAC (sesja użytkownika przenoszona jak w innych komendach). Właściciel offline ⇒ `owner_unreachable` w odpowiedzi, bez lokalnego zapisu.

### 6.2 Geometria — replikacja **pull**, bez `core.blob`
`FileBlobStore::put` nadaje losowe `uuid` i adresuje treść po sha (`blob_store.rs:126-135`), a `capture_blob` używa na stałe `DEFAULT_ORG_ID` (`blob_store.rs:102-115`); `BlobStore::gc` jest stubem (`:34-39`), a `FileBlobStore::gc` kasuje po mtime bez refcount (`:223`). Publikacja co 30 s snapshotu każdego brudnego chunku jako bloba rosłaby bez ograniczeń w blobach i w op-ach Fjall — anty-wzorzec z SPATIAL §10.2. Dlatego geometria **nie przechodzi przez blob store ani ledger**:

- `map_chunks` w ledgerze jest wyłącznie indeksem `(scene, chunk_key) → (revision, sha256, size)`.
- Replika (każdy węzeł z Sync Policy obejmującą scenę, o roli `replica` w `scene.meta`) po zmaterializowaniu wiersza z `sha256`, którego nie ma lokalnie, dodaje go do kolejki pobrań; pobiera **bi-streamem QUIC** `MESH_MSG_MAP_CHUNK_PULL` (nowy dyskryminator w `tentaflow-protocol/src/mesh.rs`, obok `MESH_MSG_LIDAR_STREAM_SUBSCRIBE = 0x53`; obsługa w `iroh_manager.rs` jak `:2586/:3016`), żądanie `{scene_id, chunk_keys: Vec<(key, sha256)>}`, odpowiedź `[u32 len][.tfmc bytes]×n` — wzorzec `mesh/recordings_pull.rs:136 pull_remote`. Źródłem jest właściciel; jeśli właściciel offline, dowolny zaufany węzeł, który w odpowiedzi `MapChunkHave` potwierdzi ten `sha256`. Weryfikacja sha po odbiorze, zapis atomowy (tmp + rename), potem `SharedMapManager::reload_chunk`.
- **GC repliki**: plik chunku jest usuwany, gdy indeks ma nowszą `revision` (po pobraniu nowszej) lub gdy scena ma `deleted_at_ms`. Właściciel usuwa starą rewizję po kompakcji. Brak innego mechanizmu GC i brak wierszy „per snapshot" — rozmiar na dysku ≈ 1 snapshot per chunk per węzeł.
- Opóźnienie repliki ≤ ~40 s od kompakcji (OK dla widoku offline); widok „na żywo" ze zdalnego węzła idzie przez relay delt (§6.4).

### 6.3 Ingest zdalny — odwrócony bi-stream, nie komenda
`MeshCommandType` to request/response z limitem 600 s (`iroh_manager.rs:2692`) i bez backpressure — nie nadaje się na 5–30 klatek/s. Zamiast tego **nowy dyskryminator bi-streamu** `MESH_MSG_MAP_INGEST_STREAM` (lustro `services/lidar_relay`: tam obserwator otwiera strumień do właściciela robota; tu **źródło** otwiera strumień do **właściciela sceny**). Po nagłówku `MapIngestOpen { scene_id, device_id, session_epoch, org_id }` źródło wysyła rekordy CBOR z prefiksem długości: `Frame(LidarStreamFrame)`, `Pose(PoseSampleV1)`, `Detections(bytes)`; właściciel odsyła tym samym strumieniem `MapIngestState { state, placement, drift_estimate, dropped }` przy każdej zmianie stanu i co 5 s. Backpressure: bounded sink jak w `lidar_relay::server::handle` — źródło przy pełnym buforze robi drop-oldest klatki (pozy nigdy nie są gubione, są małe), licznik w telemetrii. Zamknięcie strumienia = koniec sesji po stronie właściciela dopiero po 30 s bez ponownego otwarcia (reconnect zachowuje `session_epoch`).

### 6.4 Relay widoku
Subskrypcja `map:<scene_id>` na węźle innym niż właściciel: (a) snapshot z lokalnej repliki chunków (może być starszy), (b) delty na żywo przez relay strumienia z właściciela (wzorzec `register_remote_lidar_relay`, `dispatch/stream.rs:624`, ten sam mechanizm bi-streamu co `lidar_relay`). Gdy właściciel offline — (a) bez (b), UI pokazuje „ostatnia aktualizacja: …".

### 6.5 Właściciel offline i przekazanie właściciela (dwustronne)
- **Offline**: ingest wstrzymany; węzeł źródłowy buforuje ≤ 30 s klatek (potem drop), pokazuje stan `owner_unreachable`. Brak automatycznego failoveru w v1 (pytanie 4 [BLOKUJĄCE]).
- **Przekazanie** (`SceneSetOwnerRequest`, `map.admin`) jest wykonywane na **nowym** właścicielu B:
  1. Jeśli stary właściciel A jest osiągalny: B wysyła `MapOwnerHandoverPrepare` (komenda) → A wstrzymuje ingest, kompaktuje wszystkie brudne chunki, publikuje indeks, odpowiada listą `(chunk_key, revision, sha)`; B pobiera brakujące (§6.2) i dopiero wtedy zapisuje `map_scenes.owner_node_id=B, owner_epoch+1`. Brak utraty danych.
  2. Jeśli A jest nieosiągalny: admin potwierdza w UI „przejmij z możliwą utratą zmian od ostatniej kompaktacji (≤ ~40 s + WAL)"; B pisze `owner_epoch+1` na podstawie własnej repliki.
  3. **Stary właściciel po powrocie**: materializer widzi `map_scenes.owner_epoch` > lokalnego i `owner_node_id ≠ local` ⇒ `SharedMapManager::demote(scene)`: zatrzymuje ingest, zamyka sesje, **kasuje `wal.log` i każdy plik chunku, którego `(revision, sha)` nie występuje w indeksie nowej epoki**, ustawia `scene.meta.role=replica` i pobiera braki. Wiersze `map_chunks`/`map_scenes` z jego niższym `owner_epoch` są odrzucane przez materializer na wszystkich węzłach, więc jego spóźnione op-y nie nadpiszą stanu B.
- Placementy urządzeń przy zmianie właściciela pozostają ważne (są w scenie, nie w węźle); sesje urządzeń są otwierane na nowo na B (nowy `session_epoch`, relokalizacja od poprzedniego placementu jako hipotezy startowej — zwykle natychmiastowa).

### 6.6 Usuwanie sceny i lokalizacji
`SceneDeleteRequest` (`map.admin`): właściciel zamyka sesje, zapisuje `deleted_at_ms` w `map_scenes` i we wszystkich `map_device_placements`/`map_chunks` sceny (jedna transakcja), po czym usuwa `maps/<scene_id>/` lokalnie. Każdy inny węzeł w materializerze `map_scenes` z `deleted_at_ms` ⇒ `SharedMapManager::unload(scene)` + usunięcie `maps/<scene_id>/`. Właściciel offline ⇒ żądanie odrzucone (`owner_unreachable`), chyba że admin użyje „usuń mimo to" — wtedy wiersz soft-delete pisze węzeł żądający, a właściciel po powrocie zachowuje się jak każda replika (kasuje katalog). `SiteDeleteRequest` odmawia, gdy lokalizacja ma nieusunięte sceny. Fizyczne usunięcie wierszy soft-delete — istniejąca kompakcja ledgera.

## 7. Protokół i streaming

### 7.1 Nowa rodzina `MessageBody::MapBody(MapPayload)` (`tentaflow-protocol/src/message_body.rs`)
```
SiteListRequest{} / SiteListResponse{sites}
SiteUpsertRequest{site} / SiteDeleteRequest{site_id} / SiteResponse{ok,error,site}
SceneListRequest{site_id?} / SceneListResponse{scenes(+stats: voxels, chunks, last_update_ms, owner online, replica_state)}
SceneUpsertRequest{scene} / SceneDeleteRequest{scene_id, force} / SceneResponse
SceneSetOwnerRequest{scene_id,node_id, accept_loss} / SceneGeoAnchorSetRequest{scene_id, lat?,lon?,alt?,heading?}
SceneClearRegionRequest{scene_id, bbox}                       -- ręczne czyszczenie (map.write)
DeviceListRequest{scene_id?} / DeviceListResponse{devices: [(node_id,device_id,session_epoch,state,placement,method,locked,confidence,drift_estimate)]}
DeviceAssignRequest{scene_id,node_id,device_id} / DeviceUnassignRequest
DevicePlaceRequest{scene_id,node_id,device_id, pose7, method:'manual'|'icp', locked} / DeviceRelocalizeRequest{…}
DeviceResponse{ok,error,state,placement}
ViewportUpdate{stream_id, bbox}
```
Nowy wariant jest dopisany na końcu enuma (append-only, ciborium taguje po nazwie). Rodzina wymaga edycji w **trzech** miejscach, jak `RobotsBody`: `message_body.rs`, `tentaflow-protocol-wasm/src/lib.rs` (dekoder/enkoder payloadu, wzorzec `decode_robots_payload`, `lib.rs:10904`) i `www/js/protocol/codec.js` (buildery, wzorzec `codec.js:1117`). To część P0, nie „potem". Usuwamy `RobotsPayload::GeoAnchorSetRequest/GetRequest/Response` i handlery `robots_geo_anchor_*` (`dispatch/robots.rs:439-530`) oraz ich użycie w `robots.js:1417-1431` i `codec.js:1183-1220` (georeferencja jest per scena) — usunięcie wariantów zmienia kontrakt, więc wymaga podbicia `SCHEMA_VERSION` i przebudowy wszystkich węzłów razem. Handlery w nowym `dispatch/maps.rs` z makrem `#[handler(variant=…)]` + wpisy nazw w `dispatch/mod.rs`. Warianty `Anchor*` i `ChangeEvent*` dochodzą w P7 (nowy sub-enum, append-only).

### 7.2 Strumień `map:<scene_id>` (StreamHub, `BinaryStreamSource`)
Nowy typ w sdk-spec `protocol/map.rs`: `MapFrameHeader { version u8, kind u8 (0 Snapshot, 1 Delta, 2 DevicePose, 3 Barrier), flags u8 (LZ4), scene_id u64, revision u64, chunk_key i32×3, lod u8, count u32, timestamp_us i64 }`; ciało: Snapshot/Delta = planarny `u32 cell_idx[]` + `u8 state[]` (1 added, 0 removed) + `u8 source_mask[]`; DevicePose = `GlobalPoseFrame` + `device_id`. Sekwencja po subskrypcji: `Snapshot` per chunk (w kolejności odległości od środka widoku) na wybranym LOD → `Barrier{revision}` → `Delta` co ≤ 100 ms (koalescencja) + `DevicePose` ≤ 10 Hz per urządzenie → heartbeat `Barrier` co 5 s (klient porównuje `revision`; luka ⇒ prosi o resnapshot chunku przez subskrypcję z `from_revision`). Latest-wins per chunk jak dziś dla `lidar:`.
LOD: liczony **przy publikacji snapshotu** z komórek `stable` chunku (LOD 0..3: 0,05/0,1/0,2/0,4 m; komórka LOD-k zajęta, gdy ≥ 1 dziecko `stable`), cache w RAM per (chunk, revision); nic nie jest zapisywane w DB. Klient podaje budżet (`MAX_RENDER_POINTS`, dziś w `robots.js`) i bbox kamery; serwer wybiera LOD globalny taki, że Σ liczników ≤ budżet, a chunki w promieniu 15 m od kamery dostają LOD-0 (`ViewportUpdate`). Usuwamy `scene:` i `scene-depth:` (`stream.rs:38-43`, `scene_push.rs` w całości); `lidar:` (live) zostaje.
Uprawnienie streamu: `map.read`; ramki `DevicePose` są filtrowane dla sesji bez `robot.telemetry` (§9).

### 7.3 Renderer (`tentaflow-voxel-wasm`)
`set_map_points` → `map_snapshot_chunk(chunk_key, lod, cells)` + `map_delta_chunk(chunk_key, added, removed)`; bufor instancji dzielony na zakresy per chunk (free-list), sub-upload tylko brudnych zakresów; kolor: wysokość (jak dziś) + tryb „zmiany" (added < 30 s zielone, removed znikające czerwone przez 2 s) — to jest cała detekcja zmian v1 (§5); markery urządzeń (`set_device_pose(device_key, …)`). `set_overlay_points` usunięty — filtrowanie warstw po urządzeniu przez `source_mask` (bit per urządzenie sesji).

## 8. UI dashboardu (`www/js/modules/maps.js`, nowy moduł; `robots.js` traci sekcję 534-740)

1. **Lokalizacje** (`#/maps`): `tf-table` lokalizacji (nazwa, adres, liczba map, urządzenia online, ostatnia aktualizacja) + `.tf-toolbar` z `tf-searchbox`, `tf-button` „Nowa lokalizacja" → `tf-window` z `tf-input`/`tf-textarea`; usuwanie z potwierdzeniem. Widoczne przy `map.read`, edycja przy `map.admin`. Pusty stan bez `map.admin` — §2.1.
2. **Mapa** (`#/maps?site=…&scene=…`): widok wgpu (`tf-map-view` — nowy komponent, bo pojawia się w ≥ 2 modułach: mapy i karta robota), panel boczny: urządzenia (stan sesji `placed/relocalizing/unplaced/lost`, metoda, blokada, pewność, `drift_estimate` z przyciskami „Przyjmij korektę"/„Odblokuj"), statystyki (voxele, chunki, rewizja, właściciel i czy online, stan repliki, „ostatnia aktualizacja"), warstwy (`tf-toggle`: per urządzenie, zmiany, niedopasowane submapy). Gdy żadne urządzenie nie jest online: badge „offline — dane z dysku" i mapa nadal renderowana ze snapshotu. Przejęcie właściciela z nieosiągalnego A: `tf-window` z jawnym potwierdzeniem utraty (§6.5 pkt 2).
3. **Umieszczanie urządzenia**: okno „Dopasuj urządzenie": podgląd bieżącej chmury urządzenia (z `lidar:` lub z submapy `pending/`) w kolorze nałożony na mapę; kontrolki `tf-input` X/Y/Z/yaw (+ przyciski krokowe ±0,1 m/±1°, przeciąganie w widoku), przyciski „Dopasuj automatycznie (ICP)", „Zastosuj", „Relokalizuj". Pytanie 15 (2,5D czy 6DoF).
4. Karta robota (`robots.js`): pole „Lokalizacja / mapa" z `tf-select` scen + stan sesji; link do widoku mapy. Sekcja „Wspólna mapa scen" usunięta.
5. i18n: wszystkie napisy przez `maps.*` w `www/i18n/{pl,en,de,es,fr}.json`, parytet kluczy. Formy mnogie przez `{count|…|…|…}`.

Kotwice (lista + dodawanie) i tabela zdarzeń zmian — P7.

## 9. RBAC
- `map.read`: lista lokalizacji/map, subskrypcja `map:`. Role: `org_admin`, `org_operator`, `org_viewer`. `org_viewer` **nie ma** `robot.telemetry` (`migrations.rs:6263-6268`), więc dostaje mapę bez ramek `DevicePose` (pozycje robotów na żywo pozostają za `robot.telemetry`, spójnie z resztą platformy). Do potwierdzenia — pytanie 16.
- `map.write`: przypisanie/umieszczenie urządzeń, relokalizacja, czyszczenie regionu. Role: admin, operator.
- `map.admin`: lokalizacje i mapy (CRUD), właściciel sceny, georeferencja, `max_voxels`. Role: admin.
- Ingest z addonów nadal przez `lidar.publish` + nowe `robot.pose_publish_v1` (ten sam scope) — bez zmian w modelu zaufania addonu; ingest zdalny przez mesh wymaga zaufanego węzła (istniejąca polityka) **i** wiersza przypisania urządzenia do sceny (inaczej `PolicyDenied`, §2.6).
- Widoczność zdalnych scen: przez Sync Policy per `resource_type` (jak inne zasoby core).

## 10. Migracja istniejącego kodu (bez shimów)

Usunąć:
- `tentaflow-core/src/services/slam_scene.rs` (cały `SlamSceneManager`), `services/scene_push.rs`, prefiksy `scene:`/`scene-depth:` + `enforce_scene_subscribe`/`enforce_scene_depth_subscribe`/`register_local_scene_source` w `dispatch/stream.rs` (w tym `stream.rs:805` budujące `scene:<robot_id>-depth`), `RobotsPayload::GeoAnchor*` + `robots_geo_anchor_*` + `persist_geo_anchors`/`restore_geo_anchors` + `GEO_ANCHORS_SETTING` (po migracji danych, §1.1), wywołanie `on_pose` w `mesh/robot_dispatch.rs:467-474`, sekcja „Wspólna mapa scen" i `unionInto` w `robots.js`, `set_map_points`/`set_overlay_points` w `tentaflow-voxel-wasm` (zastąpione API chunkowym).
- `tentaflow-slam/src/voxel_map.rs` (`SceneVoxelMap` — semantyka replace jest błędna dla mapy trwałej), `SlamService` (`service.rs`) wraz z `MappingFrontend::ingest_posed` (jedyny konsument) — `cov_from_icp` przenosi się do `reloc.rs`. Pozostałe moduły crate'a zostają (`eskf`, `geo` w `localization.rs`; `lidar::icp` w relokalizacji; graf/submapy dla źródeł surowych).

Zmienić w miejscu:
- `addon/host_functions/lidar.rs:128` → `SharedMapManager::global().on_device_frame(...)`; nowa funkcja hosta `robot.pose_publish_v1` w `host_functions/` + wrapper w addon-sdk + wywołanie w `go2/src/lib.rs::ingest_robot_pose`.
- `host_functions/sensors.rs:99-105` (`feed_depth`) → `on_device_frame`; `services/localization.rs:91,187` → `on_device_pose` (przez `LocalizationEngine`, jak dziś).
- `addon/mod.rs:1615, 3025` — `SlamSceneManager::remove` → `SharedMapManager::device_stopped`.
- `addon/host_functions/camera.rs:499` — `CameraPatch.depth_robot_id = "<addon>-depth"` znika: `depth_robot_id` = `addon_id`, a rozdzielenie chmury depth od LiDAR w scenie daje `source_mask` (§7.3), nie osobny robot. `depth_pose_robot_id` zostaje (poza do umieszczenia).
- `camera_ingest/depth_mapping.rs:285` → `on_device_frame(kind=RawDepth, points_frame=Sensor, origin=poza kamery)`; `encode_lidar_frame` (`:604`) zostaje (format kanoniczny) z `origin` = pozycja kamery (dziś `[0,0,0]` — błąd dla carvingu); `depth_timestamp_us` (`:627`) zostaje (domena hosta, §2.7).
- `services/localization.rs` — auto geo-anchor z GNSS ustawia `map_scenes.geo_*` sceny, do której telefon jest przypisany (zamiast anchora per robot), tylko jeśli scena nie ma jeszcze georeferencji.
- `tentaflow-slam/Cargo.toml` — `rayon = { workspace = true }`.
- `UNIFIED_SLAM_ARCHITECTURE.md` §15 „option B" — dopisać odesłanie do tego planu; §16/`:508-511` uaktualnić wynikiem P0.5.

## 11. Testy

`tentaflow-slam` (jednostkowe, bez GPU):
- `occupancy::hit_threshold_requires_three_spaced_hits`, `occupancy::single_hit_never_visible`, `occupancy::miss_from_two_viewpoints_removes_cell`, `occupancy::single_bad_ray_does_not_remove_wall`, `occupancy::log_odds_clamped`, `occupancy::hits_and_first_seen_delta_saturate`.
- `carve::dda_stops_at_first_hit_in_frame` (ściana za obiektem nie jest wycinana), `carve::window_diff_marks_vanished_cells_only_inside_overlap`, `carve::mask_blocks_hits_and_misses`, `carve::odom_frame_without_pose_hits_but_no_carve`.
- `reloc::bnb_finds_pose_within_grid_on_synthetic_room`, `reloc::bnb_matches_exhaustive_search_score` (BnB nie gubi optimum), `reloc::icp_refines_to_under_5cm`, `reloc::ambiguous_corridor_is_rejected` (dwa równie dobre kandydaci), `reloc::second_confirmation_required`, `reloc::pending_submap_merges_with_placement`.
- `chunk_io::roundtrip_snapshot_crc`, `chunk_io::wal_replay_after_truncated_tail` (obcięty ostatni rekord ignorowany), `chunk_io::compaction_keeps_state_identical`, `chunk_io::corrupt_snapshot_quarantined_and_rebuilt_from_wal`, `chunk_io::old_version_rejected`.
- `dynamic::frustum_from_bbox_contains_expected_cells`, `dynamic::detection_time_window_and_extrapolation`.
- Bench (`benches/carve.rs`, `benches/reloc.rs`, criterion) na fixture z P0.5 — progi wpisane po pomiarze P0.5.

`tentaflow-core`:
- `shared_map::first_device_identity_placement`, `second_device_requires_relocalization`, `session_epoch_on_stop_hook_frame_seq_reset_pose_jump_and_odom_epoch`, `sensor_frames_without_pose_within_300ms_dropped`, `odom_frames_ingest_without_pose`, `pose_at_timestamp_used_not_latest` (regresja `fold_frame:257`), `pose_host_fn_stamps_addon_time_not_now_us` (regresja `robot_dispatch.rs:472`), `clock_offset_min_filter_and_future_frame_drop`, `manual_placement_locked_never_written_by_drift`, `persist_restart_restores_map_and_placements`, `owner_offline_pauses_ingest`, `remote_device_frame_routed_via_ingest_stream` (mock bi-stream), `demote_old_owner_wipes_unindexed_chunks`, `scene_delete_removes_dir_on_every_node`.
- `dispatch/maps`: RBAC (viewer nie może `SiteUpsert`; operator nie może `SceneSetOwner`; viewer bez `robot.telemetry` nie dostaje `DevicePose`), NotFound-masking dla scen innej organizacji, `map:` bez `map.read` → `PolicyDenied`, kolejność snapshot→barrier→delta, resnapshot po luce rewizji, `owner_command_forwarded_to_owner_node`.
- `sync`: descriptor roundtrip capture→materialize dla 4 tabel; `apply_map_chunk` odrzuca wsteczny `owner_epoch`; `chunk_pull_verifies_sha_and_replaces_older_revision`; `replica_gc_removes_superseded_and_deleted`.
- Migracje: idempotencja, seed uprawnień, migracja `GEO_ANCHORS_SETTING` → site/scene i brak klucza po migracji.
- Protokół: golden roundtrip `MapPayload` w `tentaflow-protocol`, dekoder w `tentaflow-protocol-wasm`, buildery `codec.js` (test jednostkowy jak dla `RobotsBody`).
- JS: test parytetu i18n; `tentaflow-voxel-wasm`: `map_delta_chunk` aktualizuje tylko zakres chunku (wasm-bindgen-test).
- Dane testowe: nagrania z prawdziwego Go2 (P0.5): `robot.pose_publish_v1` + `LidarFrame` (5 min spacer 20 m tam i z powrotem + osoba przechodząca + pudło dodane/usunięte) jako fixture w `tentaflow-slam/tests/fixtures/` (< 20 MB LZ4) — narzędzie CLI `tentaflow-slam-record` z hubu.

## 12. Fazy i kryteria akceptacji

**P0 — Fundament (dane, protokół, UI lokalizacji, sync metadanych).** Migracje (w tym migracja `GEO_ANCHORS_SETTING`), `MapPayload` w trzech miejscach (`message_body.rs`, `protocol-wasm`, `codec.js`), `dispatch/maps.rs`, `maps.js` (lista lokalizacji i map, CRUD, RBAC, pusty stan), deskryptory sync, i18n. Akceptacja: lokalizacja utworzona na węźle A widoczna na B w ≤ 10 s; viewer widzi listę, nie może edytować; testy RBAC i golden protokołu zielone.

**P0.5 — Sonda Go2 (bramka dla P2).** Funkcja hosta `robot.pose_publish_v1` + wywołanie w addonie Go2 + `tentaflow-slam-record`. Na prawdziwym Go2 (Air, WebRTC): nagranie z §11. Kryteria, każde z raportem w `docs/reports/`: (1) czy `rt/utlidar/robot_pose` zmienia pozycję zgodnie z przebytą drogą (błąd ≤ 10 % na 20 m) — `UNIFIED_SLAM_ARCHITECTURE.md:508-511` mówi, że przez WebRTC pozycji świata NIE ma; jeśli pozy nie ma, obowiązuje strategia z pytania 1 (okno voxeli + yaw IMU) i P0.5 dodatkowo mierzy: czy `origin` okna jest stały w układzie odometrii (klatki z różnych miejsc pokrywają się na tej samej ścianie ≤ 0,1 m) i czy środek okna śledzi przebytą drogę (błąd ≤ 10 % na 20 m); (2) czy okno Go2 usuwa zajętość (decyzja window-diff vs DDA, §3.2); (3) rozkład voxeli/klatkę i kadencja (budżet §3.2); (4) kadencja `robot_pose` (parametr `pose_at`). Bez P0.5 żadne liczby wydajności w tym planie nie są wiążące.

**P1 — Silnik zajętości w `tentaflow-slam`.** `occupancy`, `carve`, `chunk_io`, `reloc` (BnB + ICP), benchmark, fixture. Akceptacja: wszystkie testy §11 crate'a; benchmark w budżecie ustalonym w P0.5; odtwarzanie po awarii identyczne bit-w-bit ze stanem przed; uszkodzony snapshot odbudowany z WAL.

**P2 — Podmiana ścieżki ingestu w core + trwałość + nowy strumień + renderer.** `SharedMapManager`, hak `device_stopped`, placement identity/manual z blokadą, Go2 (jeśli P0.5 dało pozę) + kamera depth + telefon lokalny → jedna scena; WAL/snapshoty; `map:` snapshot+delta z LOD liczonym przy publikacji; renderer chunkowy z warstwą zmian; widok offline; usunięcie kodu z §10; usuwanie sceny. Akceptacja (na żywym urządzeniu z pozą): po restarcie core mapa wraca z dysku w ≤ 3 s dla 3 M voxeli; obiekt usunięty z pokoju znika z mapy ≤ 10 s po tym, jak sensor spojrzy w to miejsce; ściany nie „migoczą"; brak wzrostu pamięci przez 1 h statycznej sceny; dwa urządzenia w jednej scenie po ręcznym dopasowaniu pokrywają się ≤ 0,1 m na wspólnej ścianie; ręczny placement nie zmienia się w DB przez 10 min pracy korekty dryfu.

**P3 — Relokalizacja.** Sesje/epoki, BnB + ICP, prowizoryczna submapa i scalanie, korekta dryfu, UI dopasowania. Akceptacja: robot wyłączony i włączony 5 m dalej relokalizuje się ≤ 10 s z błędem ≤ 0,2 m / 3° (10/10 prób na fixture i 8/10 na żywo); przypadek niejednoznaczny kończy się stanem `relocalizing`, nigdy błędnym `placed`; start poza mapą buduje submapę i scala ją po wejściu w znany obszar.

**P4 — Dynamika.** Subskrypcja `detection_bus`, frustum, wolumeny urządzeń z offsetem zegara. Akceptacja: osoba przechodząca przez pole widzenia nie zostawia voxeli po 5 s (0 komórek `stable` w jej śladzie); pudło 40 cm postawione → widoczne jako „nowe" w warstwie zmian ≤ 10 s po obserwacji.

**P5 — Mesh.** Indeks chunków w ledgerze, pull `MESH_MSG_MAP_CHUNK_PULL`, GC repliki, strumień ingestu `MESH_MSG_MAP_INGEST_STREAM`, relay widoku, dwustronne przekazanie właściciela, `MapOwnerCommand`. Akceptacja: telefon na węźle B buduje mapę sceny węzła A; węzeł B ogląda mapę A z opóźnieniem ≤ 200 ms (delty) i ma pełną replikę ≤ 60 s po ostatniej kompaktacji; dysk repliki nie rośnie przez 1 h statycznej sceny; wyłączenie A → B nadal pokazuje mapę z „ostatnia aktualizacja"; przejęcie właściciela przez B z osiągalnym A bez utraty zmian; A po powrocie staje się repliką i nie nadpisuje niczego.

**P6 — Skala.** LOD z `ViewportUpdate`, `max_voxels` i alerty, opcjonalny kernel GPU (wgpu/CUDA) dla DDA, eksport PLY (pytanie 17). Akceptacja: scena 30 M voxeli renderuje się w budżecie klienta z LOD; 4 źródła RawDepth równolegle w budżecie CPU/GPU.

**P7 — Zdarzenia i kotwice.** `map_change_events` (klastrowanie, ack, retencja), `map_anchors` + detekcja tagów (jeśli pytanie 8 = tak), warianty protokołu w nowym sub-enumie, UI. Akceptacja: pudło postawione → zdarzenie „appeared" ≤ 10 s; zabrane → „disappeared"; zdarzenia dynamiczne domyślnie ukryte.

## 13. Ryzyka

1. **Pozycja świata Go2 przez WebRTC** — `UNIFIED_SLAM_ARCHITECTURE.md:508-511` (probe-confirmed: brak topiku pozy). Ethernet/DDS wykluczony decyzją użytkownika (brak portu, robot mobilny). Ryzyko ograniczone: mapa z Go2 nie potrzebuje pozy (klatki w układzie odometrii); pozycja do carvingu/markera z okna voxeli + yaw IMU (pytanie 1). Jeśli P0.5 pokaże, że okno nie jest centrowane na robocie ani stałe względem odometrii — odometria ICP z kolejnych okien.
2. **Wyciek wgpu** wspomniany w UNIFIED §12 jako blokada „live hook" — status nieznany (pytanie 18).
3. **Głębia monokularna** (błąd ∝ odległość) — carving z `l_miss × 0,5` i `MAX_DEPTH_M`; kamery z depth sprzętowym (telefony) są wiarygodniejsze.
4. **Zegary** — trzy źródła stempli sprowadzone do dwóch domen (host, telefon) i estymatora offsetu per urządzenie zdalne (§2.7); twarde błędy wykrywane testami `sensor_frames_without_pose_within_300ms_dropped` i `clock_offset_min_filter_and_future_frame_drop`.
5. **Dryf odometrii Go2** bez pełnego grafu póz — korekta scan-to-map (§2.5) ograniczona krokowo; długie sesje w dużych halach mogą wymagać submap/loop-closure (faza po P7).
6. **Duże mapy na węzłach-telefonach** — telefon nie ładuje chunków (tylko stream), chyba że pytanie 14 wskaże inaczej.
7. **Wydajność zapisu małych rekordów** (SPATIAL §10.2 pkt 7) — WAL batchowany 250 ms, snapshoty ≥ 30 s; benchmark P1.
8. **Utrata danych przy wymuszonym przejęciu właściciela** (§6.5 pkt 2) — ograniczona do zmian od ostatniej kompaktacji; akceptowalność — pytanie 4.
9. **Window-diff** zależy od zachowania okna Go2 (§3.2) — mierzone w P0.5, z gotową alternatywą DDA.
10. **Wiele robotów tego samego typu na węźle** — dziś nierozróżnialne na poziomie platformy (§0); plan trzyma `device_id` jako nieprzezroczysty tekst, ale nie rozwiąże tego bez zmiany identyfikacji robotów (pytanie 5).

## 14. Odpowiedź na recenzję

Każdy punkt recenzji zweryfikowany w kodzie; poniżej co przyjęto, co odrzucono i dlaczego.

**Fakty (1a–1j)** — wszystkie potwierdzone, plan poprawiony: (a) poza co 10 s z `now_us()` — nowa funkcja hosta `robot.pose_publish_v1`, usunięcie `on_pose` z `robot_dispatch.rs` (§2.7, §10); doprecyzowanie: nagłówek klatki Go2 stempluje **WASI realtime** (wall-clock hosta, `go2/src/lib.rs:242-244, 1414`), nie zegar monotoniczny, a addon działa w wasmtime na hoście core, więc Go2, `depth_mapping` i `detection_bus` dzielą jeden zegar — obcą domeną są tylko węzły telefonów (§2.7). (b) brak haka startu — sesja otwierana pierwszą klatką po `device_stopped` z `addon/mod.rs:1615/3025` (§2.2). (c) `-depth` w `camera.rs:499` (`CameraPatch.depth_robot_id`) i `stream.rs:805` — poprawione odesłania (§10). (d) `feed_pose` → `LocalizationEngine::ingest_pose` → `localization.rs:91,187` — poprawione (§2.6). (e) `protocol-wasm/src/lib.rs:10904` i `codec.js:1117` — dodane do P0 (§7.1). (f) `FileBlobStore::put` nadaje uuid, `capture_blob` używa `DEFAULT_ORG_ID` — geometria nie idzie przez blob store (§6.2). (g) `rayon` tylko w root `Cargo.toml:226` — dodany do `tentaflow-slam` (§3.2, §10). (h) „kilka tysięcy voxeli" (`lib.rs:836-838`) — budżet przeniesiony na pomiar w P0.5 (§3.2). (i) `org_viewer` bez `robot.telemetry` (`migrations.rs:6263-6268`) — decyzja: `map.read` dla viewera, `DevicePose` filtrowane (§9, pytanie 16). (j) `UNIFIED:508-511` — P0.5 jako bramka przed P2 (§12).

**Luki (2)** — przyjęte: dwa Go2 na węźle (to luka platformy, §0, pytanie 5); start poza mapą — prowizoryczna submapa (§2.4 pkt 4); bufor 15 s/8 MB (§2.2); wiele telefonów — backpressure per strumień, limit sesji przez `max_voxels` i licznik drop (§6.3); zegary (§2.7); powrót starego właściciela (§6.5 pkt 3); usuwanie (§6.6); migracja `GEO_ANCHORS_SETTING` (§1.1); autoryzacja telefonu (§2.6, pytanie 3); uszkodzony snapshot i wersje `.tfmc` (§1.2); pierwsze uruchomienie bez `map.admin` (§2.1).

**Ryzyka (3)** — przyjęte: pull zamiast blobów z indeksem w ledgerze (§6.2); odwrócony bi-stream zamiast `MeshCommandType` — potwierdzony limit 600 s (`iroh_manager.rs:2692`) i wzorzec `lidar_relay` (`MESH_MSG_LIDAR_STREAM_SUBSCRIBE = 0x53`, `iroh_manager.rs:2586/3016`) (§6.3); window-diff mierzony w P0.5 (§3.2); BnB wielorozdzielczościowy (§2.4); `pose_at` dla Go2 tylko do carvingu/markera (§3.2 pkt 1); blokada ręcznego placementu + jeden pisarz (§2.5). **Odrzucone w części**: „LWW nadpisze ręczne" — przy jednym pisarzu (właściciel) LWW nie rozstrzyga między dwiema stronami; blokada `locked` została dodana i tak, bo chroni przed korektą automatyczną tego samego pisarza.

**CLAUDE.md (4)** — przyjęte wszystkie: jedno źródło prawdy placementu, brak martwych blobów, ponowne użycie szyn `lidar_relay`/`recordings_pull`, `ts_ms` = wall-clock ms przechwycenia (`detection_bus.rs:211`), przeliczany na µs; **doprecyzowanie**: dla kamery robota lokalnego nie ma przesunięcia zegara do korygowania (ten sam host), konwersja to tylko jednostki; offset dotyczy detekcji z węzła telefonu.

**Nadmiar (5)** — przyjęte: `map_change_events` → P7, v1 = flagi w deltach (§5); kotwice/dok → P7 (§2.4); pola per komórkę 12 → 9 B (§1.2); LOD przy publikacji, bez `lod_counts` (§7.2); jedna partycja `maps` (§6.1).

**Priorytety (6)** — wszystkie „krytyczne" i „ważne" wdrożone; P0.5 dodane.

## Pytania do użytkownika

Kolejność: blokujące (bez odpowiedzi nie ma odbioru P2/P3/P5), potem projektowe, potem drobne.

1. **ROZSTRZYGNIĘTE (decyzja użytkownika):** Go2 Air nie ma Ethernetu, a robot musi być mobilny — jedynym łączem jest Wi-Fi/WebRTC; DDS odpada. Strategia pozy bez `rt/utlidar/robot_pose`: (a) klatki `voxel_map_compressed` są już w układzie odometrii Go2 (`point = idx * resolution + origin`, `go2/src/lib.rs:1106-1122`), więc do wpisania ich do sceny wystarcza `placement` sesji — pozycja robota NIE jest potrzebna do samej mapy; (b) pozycja robota do carvingu i markera = środek okna voxeli (`origin + extent/2` w XY, P0.5 weryfikuje, że okno jest centrowane na robocie) + yaw z IMU (`rt/lf/lowstate` → `imu_yaw`, `go2/src/lib.rs:1659,1731`); (c) jeśli P0.5 wykaże, że `robot_pose` jednak płynie — używamy go zamiast (b). Wariant zapasowy, gdy okno nie jest centrowane: odometria scan-to-scan ICP kolejnych okien (`lidar::icp::register`).
2. **[BLOKUJĄCE]** Robot startujący poza znaną mapą: budować prowizoryczną submapę i scalać po relokalizacji (domyślne w planie, §2.4 pkt 4), czy czekać na ręczne umieszczenie przez operatora (prostsze, ale robot „nie mapuje" do interwencji)?
3. **[BLOKUJĄCE]** Ingest z telefonu: autoryzacja przez **użytkownika** z `map.write`, który przypisuje `(node, phone)` do sceny (domyślne w planie), czy przez **admina per węzeł** (telefon-węzeł dostaje prawo pisania do sceny niezależnie od zalogowanego użytkownika)?
4. **[BLOKUJĄCE]** Właściciel sceny: automatycznie węzeł, na którym admin utworzył mapę, z ręcznym przekazaniem (§6.5) — OK? Czy akceptowalna jest utrata zmian od ostatniej kompaktacji (≤ ~40 s + WAL) przy przejęciu od nieosiągalnego właściciela, oraz wstrzymanie ingestu (bufor 30 s, potem drop), gdy właściciel jest offline — czy wymagany jest automatyczny failover (wybór lidera, dodatkowa złożoność)?
5. **[BLOKUJĄCE]** Ile robotów tego samego typu (np. dwa Go2) ma pracować na jednym węźle? Dziś platforma kluczuje robota po `addon_id`, więc dwa Go2 są nierozróżnialne — jeśli „więcej niż jeden", identyfikacja robotów musi się zmienić poza tym planem.
6. Czy jedna lokalizacja ma mieć dokładnie jedną mapę, czy dopuszczamy wiele map w lokalizacji (np. piętra, hale)? Plan zakłada „wiele", domyślnie tworząc jedną.
7. Rozdzielczość voxela: 5 cm globalnie (jak Go2), czy konfigurowalna per mapa (np. 10 cm dla hal)? Zmiana rozdzielczości istniejącej mapy wymaga jej przebudowy od zera — czy to akceptowalne?
8. Znaczniki fizyczne (P7): czy w ogóle chcesz kotwice AprilTag/QR (wymaga druku i detekcji tagów w kamerach — dodatkowy moduł), czy wyłącznie relokalizację bez markerów + dopasowanie ręczne? Jeśli tak — AprilTag (dokładniejszy) czy QR (łatwiejszy)?
9. Klasy dynamiczne do wykluczania: proponuję `person, animal, dog, cat, robot, vehicle, bicycle` — potwierdzić/uzupełnić. Czy detekcje będą dostępne również dla kamer telefonów (na węźle telefonu), czy tylko dla kamer robotów?
10. Limity: maksymalny rozmiar jednej mapy (proponuję 50 mln voxeli ≈ 450 MB RAM) i spodziewana liczba lokalizacji/map jednocześnie aktywnych na jednym węźle? Ile telefonów naraz ma skanować jedną scenę?
11. Potwierdzić usunięcie georeferencji per robot (`RobotGeoAnchor*`) na rzecz jednej georeferencji per mapa oraz migrację istniejących kotwic do sceny „Zmigrowane: <robot_id>" (§1.1) — czy są w ogóle produkcyjne dane w `GEO_ANCHORS_SETTING`, czy można je po prostu porzucić?
12. Zmiany: czy obiekt ma „znikać" po pierwszym spełnieniu progu (§3.1: ≥ 3 obserwacje wolnej przestrzeni z 2 pozycji), czy wymagać wielokrotnych potwierdzeń rozłożonych w czasie (np. w 3 różnych dniach)? Ile dni przechowywać zdarzenia zmian (P7)? Czy potrzebna historia mapy („stan z dnia X") — wielokrotnie zwiększa storage i nie jest w planie.
13. GPU: czy akceptujesz podejście CPU-first (rayon) z kernelem GPU dopiero, gdy benchmark P1 nie zmieści się w budżecie, czy wymagasz ray-castingu na GPU od razu?
14. Telefony: czy telefon ma tylko streamować widok mapy z węzła-właściciela (domyślne), czy również trzymać lokalną replikę chunków (offline w terenie)? Jeśli replika — jaki limit rozmiaru na telefonie (np. 200 MB) i co robić po jego przekroczeniu (tylko chunki w promieniu N m od ostatniej pozycji)?
15. Ręczne dopasowanie urządzenia: wystarczy 2,5D (X, Y, yaw + wysokość podłogi z automatu), czy potrzebne pełne 6DoF (roll/pitch — np. dla dronów)?
16. RBAC: `org_viewer` dostaje `map.read` (widzi mapę bez pozycji urządzeń na żywo, bo nie ma `robot.telemetry`) — OK, czy mapa ma być tylko dla operatora i admina?
17. Kto może ręcznie czyścić obszar mapy: operator (`map.write`) czy tylko admin? Czy eksport mapy (PLY/glTF) jest wymagany w tym zakresie (P6), czy później?
18. Widok: czy domyślnie ukrywać sufit (przycięcie Z powyżej np. 2,2 m nad podłogą) i czy potrzebny jest widok 2D (rzut z góry) obok 3D? Jaki jest aktualny status naprawy wycieku wgpu, który UNIFIED §12 wskazuje jako blokadę uruchomienia SLAM na żywo?
