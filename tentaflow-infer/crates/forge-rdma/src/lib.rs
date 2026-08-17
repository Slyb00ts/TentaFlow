// ===== File: lib.rs — transport RoCE miedzy wezlami =====
//
// Model 156 GB nie miesci sie w 121 GB jednego GB10, wiec podzial na dwa wezly
// nie jest optymalizacja, tylko warunkiem uruchomienia. `cluster.rs` obsluguje
// karty W JEDNYM pudle po P2P; ten modul dokłada brakujaca warstwe miedzy
// pudlami.
//
// RDMA jest tu potrzebna po to, zeby CPU wypadl ze sciezki danych: NIC pisze
// wprost do pamieci. Zwykle gniazda przepuscilyby kazdy bajt przez stos jadra
// i kopie hosta.
//
// Czym pamiec zunifikowana GB10 (`integrated == 1`) POMAGA, a czym nie —
// zmierzone, bo intuicja tu myli:
//
//   cuMemAlloc (pule FORGE)      -> ibv_reg_mr ODMAWIA
//   cuMemAllocManaged            -> ibv_reg_mr ODMAWIA
//   cuMemHostAlloc + DEVICEMAP   -> ibv_reg_mr OK i wskaznik urzadzenia dziala
//   dmabuf z cuMemAlloc          -> ODMAWIA (eksport wymaga VMM cuMemCreate)
//
// Czyli: NIE da sie zarejestrowac istniejacych pul. Bufor ladowania musi byc
// osobna klasa alokacji. Za to gdy juz nia jest, `integrated` placi naprawde —
// bajty wladowane przez NIC sa widoczne dla kerneli BEZ zadnej kopii, podczas
// gdy klaster z osobna pamiecia karty musi tu robic GPUDirect albo staging.
//
// Dlatego `Link` NIE alokuje sam: dostaje wskaznik i dlugosc od wolajacego,
// ktory wie, jak ta pamiec powstala. Inaczej crate zgadywalby klase alokacji.
//
// Wymiana adresow idzie po zwyklym TCP: jednorazowy uscisk dloni, po ktorym
// caly ruch danych omija jadro.

use forge_types::{ForgeError, Result};
use std::ffi::{c_char, c_int, c_void, CString};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream, ToSocketAddrs};

#[repr(C)]
struct Ep {
    _private: [u8; 0],
}

#[repr(C)]
struct Mr {
    _private: [u8; 0],
}

/// Adres punktu koncowego wymieniany przy uscisku dloni.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct PeerAddr {
    pub qpn: u32,
    pub lid: u16,
    pub gid: [u8; 16],
    /// Adres bufora, do ktorego druga strona bedzie pisac.
    pub remote_buf: u64,
    pub rkey: u32,
}

impl PeerAddr {
    const WIRE: usize = 4 + 2 + 16 + 8 + 4;

    fn to_wire(self) -> [u8; Self::WIRE] {
        let mut b = [0u8; Self::WIRE];
        b[0..4].copy_from_slice(&self.qpn.to_le_bytes());
        b[4..6].copy_from_slice(&self.lid.to_le_bytes());
        b[6..22].copy_from_slice(&self.gid);
        b[22..30].copy_from_slice(&self.remote_buf.to_le_bytes());
        b[30..34].copy_from_slice(&self.rkey.to_le_bytes());
        b
    }

    fn from_wire(b: &[u8; Self::WIRE]) -> Self {
        let mut gid = [0u8; 16];
        gid.copy_from_slice(&b[6..22]);
        Self {
            qpn: u32::from_le_bytes(b[0..4].try_into().unwrap()),
            lid: u16::from_le_bytes(b[4..6].try_into().unwrap()),
            gid,
            remote_buf: u64::from_le_bytes(b[22..30].try_into().unwrap()),
            rkey: u32::from_le_bytes(b[30..34].try_into().unwrap()),
        }
    }
}

extern "C" {
    fn forge_rdma_open(dev: *const c_char, port: u8) -> *mut Ep;
    fn forge_rdma_local_addr(ep: *mut Ep, out: *mut PeerAddr);
    fn forge_rdma_reg(ep: *mut Ep, buf: *mut c_void, len: usize) -> *mut Mr;
    fn forge_rdma_rkey(mr: *mut Mr) -> u32;
    fn forge_rdma_connect(ep: *mut Ep, peer: *const PeerAddr) -> c_int;
    fn forge_rdma_write(
        ep: *mut Ep,
        local: *mut Mr,
        local_off: u64,
        remote_addr: u64,
        rkey: u32,
        len: u32,
        wr_id: u64,
        signaled: c_int,
    ) -> c_int;
    fn forge_rdma_poll(ep: *mut Ep, max: c_int, out_status: *mut c_int) -> c_int;
    fn forge_rdma_close(ep: *mut Ep);
}

/// Polaczenie RC z jednym sasiadem plus zarejestrowany bufor lokalny.
pub struct Link {
    ep: *mut Ep,
    mr: *mut Mr,
    buf: *mut u8,
    len: usize,
    peer: PeerAddr,
}

// Uchwyty verbs sa zwiazane z procesem, nie z watkiem; `Link` wlada nimi
// wylacznie i nie wystawia wnetrza.
unsafe impl Send for Link {}

impl Link {
    /// Otwiera HCA, rejestruje bufor wolajacego i wymienia adresy z sasiadem.
    ///
    /// # Bezpieczenstwo
    /// `buf` musi zyc co najmniej tak dlugo jak `Link` i miec `len` bajtow.
    /// Musi tez pochodzic z alokacji, ktora `ibv_reg_mr` przyjmuje — na tej
    /// maszynie `cuMemHostAlloc` albo zwykle strony hosta; `cuMemAlloc` i
    /// `cuMemAllocManaged` sa ODRZUCANE (patrz naglowek pliku).
    ///
    /// Strona `listen` czeka, strona laczaca sie dzwoni — obie musza podac ten
    /// sam rozmiar bufora, bo adres i `rkey` opisuja caly zarejestrowany obszar.
    pub unsafe fn bind(
        device: &str,
        port: u8,
        buf: *mut u8,
        len: usize,
        addr: &str,
        listen: bool,
    ) -> Result<Self> {
        let dev = CString::new(device)
            .map_err(|_| ForgeError::Device("nazwa urzadzenia RDMA z bajtem zerowym".into()))?;
        let ep = unsafe { forge_rdma_open(dev.as_ptr(), port) };
        if ep.is_null() {
            return Err(ForgeError::Device(format!(
                "nie udalo sie otworzyc HCA {device} portu {port}"
            )));
        }

        let mr = unsafe { forge_rdma_reg(ep, buf as *mut c_void, len) };
        if mr.is_null() {
            unsafe { forge_rdma_close(ep) };
            return Err(ForgeError::Device(
                "rejestracja pamieci odrzucona: alokacja urzadzenia (cuMemAlloc/Managed) \
                 albo limit zablokowanych stron"
                    .into(),
            ));
        }

        let mut local = PeerAddr::default();
        unsafe { forge_rdma_local_addr(ep, &mut local) };
        local.remote_buf = buf as u64;
        local.rkey = unsafe { forge_rdma_rkey(mr) };

        let peer = Self::handshake(addr, listen, local)?;
        if unsafe { forge_rdma_connect(ep, &peer) } != 0 {
            unsafe { forge_rdma_close(ep) };
            return Err(ForgeError::Device(
                "przejscie kolejki w RTR/RTS nieudane".into(),
            ));
        }
        Ok(Self {
            ep,
            mr,
            buf,
            len,
            peer,
        })
    }

    /// Jednorazowa wymiana adresow po TCP. Obie strony wysylaja swoj adres, po
    /// czym czytaja adres drugiej — kolejnosc jest symetryczna, wiec ta sama
    /// funkcja obsluguje obie role.
    fn handshake(addr: &str, listen: bool, local: PeerAddr) -> Result<PeerAddr> {
        let mut stream = if listen {
            let l = TcpListener::bind(addr)
                .map_err(|e| ForgeError::Device(format!("nasluch {addr}: {e}")))?;
            l.accept()
                .map_err(|e| ForgeError::Device(format!("przyjecie polaczenia: {e}")))?
                .0
        } else {
            let target = addr
                .to_socket_addrs()
                .map_err(|e| ForgeError::Device(format!("adres {addr}: {e}")))?
                .next()
                .ok_or_else(|| ForgeError::Device(format!("adres {addr} nie ma wpisu")))?;
            TcpStream::connect(target)
                .map_err(|e| ForgeError::Device(format!("polaczenie {addr}: {e}")))?
        };
        stream
            .set_nodelay(true)
            .map_err(|e| ForgeError::Device(format!("nodelay: {e}")))?;
        stream
            .write_all(&local.to_wire())
            .map_err(|e| ForgeError::Device(format!("wyslanie adresu: {e}")))?;
        let mut raw = [0u8; PeerAddr::WIRE];
        stream
            .read_exact(&mut raw)
            .map_err(|e| ForgeError::Device(format!("odbior adresu: {e}")))?;
        Ok(PeerAddr::from_wire(&raw))
    }

    /// Bufor, ktory druga strona zapisuje.
    ///
    /// # Bezpieczenstwo
    /// Sasiad moze pisac tu w kazdej chwili, wiec odczyt jest poprawny dopiero
    /// po synchronizacji na wyzszym poziomie (np. po `wait` na jego zapis).
    pub unsafe fn buffer(&self) -> &[u8] {
        std::slice::from_raw_parts(self.buf, self.len)
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Zapis `len` bajtow spod `off` naszego bufora pod ten sam offset u sasiada.
    ///
    /// `signaled` decyduje o wpisie do kolejki ukonczen — przy strumieniu
    /// zapisow sygnalizuje sie co n-ty, inaczej CQ sie przepelnia.
    pub fn write(&self, off: u64, len: u32, wr_id: u64, signaled: bool) -> Result<()> {
        let rc = unsafe {
            forge_rdma_write(
                self.ep,
                self.mr,
                off,
                self.peer.remote_buf + off,
                self.peer.rkey,
                len,
                wr_id,
                signaled as c_int,
            )
        };
        if rc != 0 {
            return Err(ForgeError::Device(format!("post_send zwrocil {rc}")));
        }
        Ok(())
    }

    /// Zbiera ukonczenia; blad transportu zwraca kod statusu verbs.
    pub fn poll(&self, max: i32) -> Result<i32> {
        let mut status: c_int = 0;
        let n = unsafe { forge_rdma_poll(self.ep, max, &mut status) };
        if n < 0 {
            return Err(ForgeError::Device("poll_cq nieudany".into()));
        }
        if status != 0 {
            return Err(ForgeError::Device(format!(
                "ukonczenie ze statusem {status}"
            )));
        }
        Ok(n)
    }

    /// Czeka na `count` ukonczen, kręcąc się w miejscu — na tej sciezce chodzi
    /// o opoznienie, wiec nie ma tu usypiania.
    pub fn wait(&self, count: i32) -> Result<()> {
        let mut left = count;
        while left > 0 {
            left -= self.poll(left)?;
        }
        Ok(())
    }
}

impl Drop for Link {
    fn drop(&mut self) {
        unsafe { forge_rdma_close(self.ep) };
    }
}
