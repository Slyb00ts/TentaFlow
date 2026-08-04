// ===== File: link_probe.rs — ile naprawde daje lacze miedzy Sparkami =====
//
// Podzial modelu na dwa wezly ma sens tylko wtedy, gdy koszt przeslania
// aktywacji jest maly wobec czasu warstwy. `topology::tensor_parallel_viable`
// podejmuje te decyzje ILOSCIOWO, wiec potrzebuje prawdziwych liczb tego lacza,
// a nie nazwy transportu.
//
// Mierzy dwie rzeczy, bo obie wchodza do tej decyzji:
//   opoznienie  — polowa czasu tam i z powrotem dla malego zapisu; tyle kosztuje
//                 kazda granica warstwy przy podziale tensorowym,
//   przepustowosc — strumien duzych zapisow; tyle daje przeslanie aktywacji.
//
// Uruchomienie (najpierw strona nasluchujaca):
//   spark-002$ link_probe --listen --bind 0.0.0.0:18515 --dev roceP2p1s0f0
//   spark-001$ link_probe --connect 10.10.10.25:18515 --dev roceP2p1s0f0

use forge_rdma::Link;
use std::time::Instant;

const BUF: usize = 256 << 20;
const PING: u32 = 64;
const ROUNDS: usize = 2000;

fn arg(name: &str) -> Option<String> {
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        if a == name {
            return it.next();
        }
    }
    None
}

fn flag(name: &str) -> bool {
    std::env::args().any(|a| a == name)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dev = arg("--dev").unwrap_or_else(|| "roceP2p1s0f0".to_string());
    let port: u8 = arg("--port").and_then(|p| p.parse().ok()).unwrap_or(1);
    let listen = flag("--listen");
    let addr = if listen {
        arg("--bind").unwrap_or_else(|| "0.0.0.0:18515".to_string())
    } else {
        arg("--connect").ok_or("podaj --connect host:port albo --listen")?
    };

    // Zwykle strony hosta: `ibv_reg_mr` je przyjmuje. Pule urzadzenia
    // (`cuMemAlloc`) sa odrzucane — patrz naglowek `lib.rs`.
    let mut buf = vec![0u8; BUF];
    let link = unsafe { Link::bind(&dev, port, buf.as_mut_ptr(), BUF, &addr, listen)? };
    println!("polaczone przez {dev} port {port}");

    // KAZDY kierunek ma wlasna skrzynke. Przy wspolnym offsecie obie strony
    // pisza pod ten sam adres i kasuja sobie licznik, a strona mierzaca czeka
    // na komorke, ktora sama przed chwila ustawila — petla konczy sie od razu
    // i "opoznienie" wychodzi zerowe.
    const TO_B: u64 = 0;
    const TO_A: u64 = 4096;
    // Strumien przepustowosci leci OBOK skrzynek: 64 MiB zapisane spod zera
    // przykryloby licznik sasiada razem z wartownikiem konca.
    const BULK: u64 = 8 << 20;
    const DONE: u64 = u64::MAX;
    let (inbox, outbox) = if listen { (TO_B, TO_A) } else { (TO_A, TO_B) };

    let base = buf.as_mut_ptr();
    // Do skrzynki odbiorczej pisze sasiad przez DMA, wiec czytamy ja ulotnie —
    // inaczej kompilator ma prawo podniesc odczyt ponad petle.
    let inbox_ptr = unsafe { base.add(inbox as usize) as *const u64 };
    let outbox_ptr = unsafe { base.add(outbox as usize) as *mut u64 };
    let recv = || unsafe { std::ptr::read_volatile(inbox_ptr) };
    let send = |v: u64| unsafe { std::ptr::write_volatile(outbox_ptr, v) };

    if listen {
        // Odbijacz: kreci sie, az zobaczy nowy licznik, i odsyla go z powrotem.
        let mut last = 0u64;
        for _ in 0..ROUNDS {
            loop {
                let v = recv();
                if v != last {
                    last = v;
                    break;
                }
                std::hint::spin_loop();
            }
            send(last);
            link.write(outbox, PING, 1, true)?;
            link.wait(1)?;
        }
        // Kolejka musi zyc przez caly pomiar przepustowosci: RDMA WRITE nie
        // budzi zdalnego CPU, ale pisze do ISTNIEJACEGO QP. Wyjscie tutaj
        // zrywalo polaczenie i klient dostawal RETRY_EXC_ERR.
        println!("odbicia zakonczone, trzymam kolejke do konca pomiaru");
        while recv() != DONE {
            std::hint::spin_loop();
        }
        println!("wartownik konca odebrany");
    } else {
        // Rozgrzewka, potem pomiar opoznienia tam i z powrotem.
        for i in 1..=64u64 {
            send(i);
            link.write(outbox, PING, 1, true)?;
            link.wait(1)?;
            while recv() != i {
                std::hint::spin_loop();
            }
        }
        let t0 = Instant::now();
        for i in 65..(65 + ROUNDS as u64 - 64) {
            send(i);
            link.write(outbox, PING, 1, true)?;
            link.wait(1)?;
            while recv() != i {
                std::hint::spin_loop();
            }
        }
        let n = ROUNDS as u64 - 64;
        let rtt = t0.elapsed().as_secs_f64() / n as f64;
        println!("opoznienie {PING} B: {:.2} us w jedna strone", rtt * 1e6 / 2.0);

        // Przepustowosc: kolejka duzych zapisow, sygnalizowany co 32-gi, zeby
        // kolejka ukonczen nie stala sie waskim gardlem pomiaru.
        for &mb in &[1usize, 4, 16, 64] {
            let len = mb << 20;
            let iters = (2usize << 30) / len;
            let t = Instant::now();
            let mut outstanding = 0;
            for i in 0..iters {
                let sig = i % 32 == 31;
                link.write(BULK, len as u32, i as u64, sig)?;
                if sig {
                    outstanding += 1;
                    if outstanding >= 4 {
                        link.wait(1)?;
                        outstanding -= 1;
                    }
                }
            }
            link.wait(outstanding)?;
            let secs = t.elapsed().as_secs_f64();
            let gb = (iters * len) as f64 / 1e9;
            println!("zapis {mb:3} MiB: {:.1} GB/s", gb / secs);
        }
        send(DONE);
        link.write(outbox, 8, 99, true)?;
        link.wait(1)?;
    }
    Ok(())
}
