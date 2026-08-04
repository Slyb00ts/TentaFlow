// ===== File: build.rs — kompilacja warstwy C nad libibverbs =====
//
// Cala obsluga kolejek siedzi w C, bo `ibv_post_send`, `ibv_poll_cq` i reszta
// goracej sciezki to funkcje INLINE w `verbs.h` — wolane sa przez tablice
// `context->ops`. Zwiazanie ich z Rusta wymagaloby odtworzenia ukladu struktur
// libibverbs co do bajtu i trzymania go w zgodzie z wersja biblioteki. Naglowek
// jest jedynym zrodlem prawdy o tym ukladzie, wiec kod, ktory go potrzebuje,
// zostaje po stronie C.
fn main() {
    println!("cargo:rerun-if-changed=src/shim.c");
    cc::Build::new()
        .file("src/shim.c")
        .flag_if_supported("-O2")
        .compile("forge_rdma_shim");
    println!("cargo:rustc-link-lib=ibverbs");
}
