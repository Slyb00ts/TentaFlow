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

    // Pierwsze slowo bufora sluzy za odbicie: strona A pisze licznik, strona B
    // widzi zmiane i odsyla. Tylko jedna strona mierzy, druga odbija.
    let cell = unsafe { std::slice::from_raw_parts(link.buffer().as_ptr(), 8) };
    let mine = unsafe { std::slice::from_raw_parts_mut(buf.as_mut_ptr() as *mut u64, 1) };

    if listen {
        // Odbijacz: kreci sie, az zobaczy nowy licznik, i odsyla go z powrotem.
        let mut last = 0u64;
        for _ in 0..ROUNDS {
            loop {
                let v = u64::from_le_bytes(cell[0..8].try_into().unwrap());
                if v != last {
                    last = v;
                    break;
                }
                std::hint::spin_loop();
            }
            mine[0] = last;
            link.write(0, PING, 1, true)?;
            link.wait(1)?;
        }
        println!("odbicia zakonczone");
    } else {
        // Rozgrzewka, potem pomiar opoznienia tam i z powrotem.
        for i in 1..=64u64 {
            mine[0] = i;
            link.write(0, PING, 1, true)?;
            link.wait(1)?;
            while u64::from_le_bytes(cell[0..8].try_into().unwrap()) != i {
                std::hint::spin_loop();
            }
        }
        let t0 = Instant::now();
        for i in 65..(65 + ROUNDS as u64 - 64) {
            mine[0] = i;
            link.write(0, PING, 1, true)?;
            link.wait(1)?;
            while u64::from_le_bytes(cell[0..8].try_into().unwrap()) != i {
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
                link.write(0, len as u32, i as u64, sig)?;
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
    }
    Ok(())
}
