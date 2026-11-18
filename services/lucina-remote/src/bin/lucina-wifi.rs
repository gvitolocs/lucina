//! lucina-wifi: asks Juno's kernel for Wi-Fi networks and connects to one, through `/dev/lucina-wifi`
//! (kernel patch 0626, `device::qcom_wifi::control`).
//!
//! ```text
//! lucina-wifi status
//! lucina-wifi scan                 waits for the scan and lists what it found
//! lucina-wifi connect [file]       waits until the network is authorised
//! lucina-wifi connect --stdin      the same, with the credentials from stdin
//! lucina-wifi connect --saved      the same, from the initramfs or the disk; an error if neither has any (the boot)
//! lucina-wifi disconnect
//! lucina-wifi save --stdin         keeps the credentials, for the next boot; the only way they are written down
//! lucina-wifi saved                says whether there are any; never what they are
//! lucina-wifi forget               removes them
//! lucina-wifi migrate              moves them from the EFI partition to `/data`
//! ```
//!
//! The credentials are `psk=<passphrase, or 64 hex digits>` and, if given, `ssid=<name>` and `freq=<MHz of the channel it was
//! last joined on>`, one per line (`esp::Credentials`): from `/etc/lucina/wifi.conf` (`scripts/remote-initramfs.sh` puts
//! `GBGO_WIFI_PSK` and `GBGO_WIFI_SSID` there, mode 600), another file, or stdin. This program never prints the passphrase, and
//! the kernel keeps it only until it has derived the key.
//!
//! A name that is saved goes out at once, with its channel if one is saved: the kernel holds the command until the firmware is
//! up, joins from what it has heard and, at worst, scans that one channel. Nothing on this path writes the store; `save` is the
//! only command that does, and `save` is a person's command. The channel a join ended on is kept in this program's memory for
//! the attempts it still has to make, and written down by no one.
//!
//! A join that will not come is tried `MAX_ATTEMPTS` times over at most `MAX_TOTAL`, with the waits wpa_supplicant waits
//! (2, 4, 8, 16 s and then 30 s); when a bound is reached the program says so once and ends with a failure, which is where
//! `lucina-watchd` takes over: it is the one that restarts Wi-Fi later, so this is not a loop that runs until the battery is
//! empty. A saved network that is not heard gives way to no other network: the only other thing tried is another saved
//! network.
//!
//! Without a name it takes the strongest network whose name starts with `FRITZ!Box` among those the kernel's own first sweep
//! heard, asking for a scan of its own only if that heard none, and says which; if none matches it lists what it heard and
//! stays disconnected.
//!
//! Its lines go to the kernel log as well as to stdout, each with the seconds since the program started: the boot runs this in
//! the background with nobody watching, and the log is where the time line of a boot is read afterwards.

// Juno with no C library: Eyra is the libc and the program start, in Rust (docs/research/juno-pure-rust.md).
#[cfg(all(target_os = "linux", target_env = "gnu", target_arch = "aarch64"))]
extern crate eyra;

use lucina_remote::esp::{self, Credentials};
use std::{
    fs,
    io::{self, Read, Write},
    process::ExitCode,
    sync::OnceLock,
    thread::sleep,
    time::{Duration, Instant},
};

const DEVICE: &str = "/dev/lucina-wifi";
const CONF: &str = "/etc/lucina/wifi.conf";
const FRITZ: &[u8] = b"FRITZ!Box";
const DISK_WAIT: Duration = Duration::from_secs(120);
/// How long a `connect` may wait for the node to appear: the first seconds of a boot, before the Wi-Fi driver has made it.
const DEVICE_WAIT: Duration = Duration::from_secs(30);
/// How many times the boot's join is tried, and how long it may take all told. Both bounds end the program with a failure and
/// one log line: `lucina-watchd` is what restarts Wi-Fi after that, and a machine with no Wi-Fi to reach is better off saying
/// so than sweeping the bands for the rest of the day.
const MAX_ATTEMPTS: u32 = 5;
const MAX_TOTAL: Duration = Duration::from_secs(600);

fn hex(data: &[u8]) -> String {
    data.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(text: &str) -> Vec<u8> {
    (0..text.len() / 2)
        .filter_map(|i| u8::from_str_radix(text.get(i * 2..i * 2 + 2)?, 16).ok())
        .collect()
}

/// The two ends of the Wi-Fi device: a line written, the status read. The tests put a model in its place, so that the order of
/// the commands and the words of the time line are checked without a kernel. A radio also carries what the program says about
/// itself, so a test sees the very lines the boot's log gets.
trait Radio {
    fn write(&self, line: &str) -> io::Result<()>;
    fn read(&self) -> io::Result<String>;

    fn tell(&self, line: &str) {
        tell(line);
    }
}

struct Device;

impl Radio for Device {
    fn write(&self, line: &str) -> io::Result<()> {
        fs::OpenOptions::new()
            .write(true)
            .open(DEVICE)?
            .write_all(line.as_bytes())
    }

    fn read(&self) -> io::Result<String> {
        fs::read_to_string(DEVICE)
    }
}

fn command(line: &str) -> io::Result<()> {
    Device.write(line)
}

fn status() -> io::Result<String> {
    Device.read()
}

fn field<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    text.lines()
        .find_map(|l| l.strip_prefix(key)?.strip_prefix(' '))
}

/// The status with SSIDs as text. A network name is whatever bytes the access point sent, so anything unprintable stays hex.
fn readable(text: &str) -> String {
    let mut out = String::new();
    for line in text.lines() {
        match line.strip_prefix("bss ") {
            Some(rest) => {
                let mut w: Vec<&str> = rest.split(' ').collect();
                let name = w.pop().unwrap_or("");
                let raw = unhex(name);
                let shown = match std::str::from_utf8(&raw) {
                    Ok(s) if !s.chars().any(char::is_control) => s.to_string(),
                    _ => format!("hex:{name}"),
                };
                out.push_str(&format!("{}  {}\n", w.join(" "), shown));
            }
            None => {
                out.push_str(line);
                out.push('\n');
            }
        }
    }
    out
}

/// How many networks the status lists. A time line says this, because a join that waits waits for a table that is filling or is
/// not, and that is the difference.
fn networks(status: &str) -> usize {
    status.lines().filter(|l| l.starts_with("bss ")).count()
}

/// Waits until the `state` line is one of `wanted` or a `failed` line appears. Says in the kernel log what it waits for, every
/// 5 s that it still does, and when it got what it wanted, so that a boot's log shows this program's own time line.
fn wait_for(
    radio: &dyn Radio,
    what: &str,
    wanted: &[&str],
    limit: Duration,
) -> Result<String, String> {
    let started = Instant::now();
    let mut told = Duration::ZERO;
    let mut polls = 0u32;
    radio.tell(&format!(
        "{:.1} s: waiting for {what} ({})",
        since_start(),
        wanted.join(" or ")
    ));
    loop {
        let text = radio.read().map_err(|err| format!("{DEVICE}: {err}"))?;
        polls += 1;
        let state = field(&text, "state").unwrap_or("none");
        if wanted.contains(&state) {
            radio.tell(&format!(
                "{:.1} s: {what}: state {state} after {:.1} s and {polls} reads, {} networks",
                since_start(),
                started.elapsed().as_secs_f64(),
                networks(&text)
            ));
            return Ok(text);
        }
        if let Some(why) = field(&text, "failed") {
            radio.tell(&format!(
                "{:.1} s: {what}: failed after {:.1} s: {why}",
                since_start(),
                started.elapsed().as_secs_f64()
            ));
            return Err(why.to_string());
        }
        if started.elapsed() > limit {
            return Err(format!(
                "no answer in {} s:\n{}",
                limit.as_secs(),
                readable(&text)
            ));
        }
        if started.elapsed() >= told + Duration::from_secs(5) {
            told = started.elapsed();
            radio.tell(&format!(
                "{:.1} s: still waiting for {what}: state {state}, {} networks, {polls} reads",
                since_start(),
                networks(&text)
            ));
        }
        sleep(Duration::from_millis(200));
    }
}

/// From a status: the strongest BSS whose name starts with `FRITZ!Box`, as (name, dBm), and how many matched.
fn pick_fritz(status: &str) -> Option<(Vec<u8>, i32, usize)> {
    let mut best: Option<(Vec<u8>, i32)> = None;
    let mut matched = 0;
    for line in status.lines() {
        let Some(rest) = line.strip_prefix("bss ") else {
            continue;
        };
        let w: Vec<&str> = rest.split(' ').collect();
        let [_, _, dbm, _, name] = w[..] else {
            continue;
        };
        let (name, Ok(dbm)) = (unhex(name), dbm.parse::<i32>()) else {
            continue;
        };
        if name.starts_with(FRITZ) {
            matched += 1;
            if best.as_ref().is_none_or(|(_, b)| dbm > *b) {
                best = Some((name, dbm));
            }
        }
    }
    best.map(|(name, dbm)| (name, dbm, matched))
}

/// What `/dev/kmsg` takes as one record: one write is one record, so the prefix, the text and the newline go in together.
fn kmsg_record(line: &str) -> String {
    format!("lucina-wifi: {line}\n")
}

/// Seconds since this program started, for the lines that say how long a step took.
fn since_start() -> f64 {
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_secs_f64()
}

/// A line in the kernel log as well as here: this runs in the background at boot, with nobody watching its output. The kernel
/// log comes first and in one write: a console that is slow to take the text on stdout must not hold up the record, or what the
/// program does next. Nothing of the passphrase is ever in these lines; the SSID is, and is safe to say.
fn tell(line: &str) {
    if let Ok(mut log) = fs::OpenOptions::new().write(true).open("/dev/kmsg") {
        let _ = log.write_all(kmsg_record(line).as_bytes());
    }
    println!("{line}");
}

/// The name to join when the credentials give none. The kernel's first sweep is in the status `wait_for` returns: the boot's
/// join waits for that table and reads it, and only asks for a scan of its own if the sweep heard no such network.
fn find_network(radio: &dyn Radio) -> Result<Vec<u8>, String> {
    let mut status = wait_for(
        radio,
        "the first sweep",
        &["idle", "connected"],
        Duration::from_secs(180),
    )?;
    if pick_fritz(&status).is_none() {
        radio.tell(&format!(
            "{:.1} s: no FRITZ!Box among the {} networks heard: asking for a scan",
            since_start(),
            networks(&status)
        ));
        radio
            .write("scan\n")
            .map_err(|err| format!("{DEVICE}: {err}"))?;
        sleep(Duration::from_millis(300));
        status = wait_for(
            radio,
            "the scan",
            &["idle", "connected"],
            Duration::from_secs(60),
        )?;
    }
    match pick_fritz(&status) {
        Some((name, dbm, matched)) => {
            radio.tell(&format!(
                "taking {} at {dbm} dBm, the strongest of {matched} FRITZ!Box entries",
                String::from_utf8_lossy(&name)
            ));
            Ok(name)
        }
        None => {
            radio.tell(&format!(
                "no FRITZ!Box network heard, staying disconnected:
{}",
                readable(&status)
            ));
            Err("no FRITZ!Box network".to_string())
        }
    }
}

/// A WPA passphrase is 8 to 63 printable characters, and a PSK 64 hex digits. Better to hear it now than at every boot.
fn check_psk(psk: &[u8]) -> Result<(), String> {
    let printable = psk.iter().all(|b| (0x20..0x7f).contains(b));
    let fits =
        (8..=63).contains(&psk.len()) || (psk.len() == 64 && psk.iter().all(u8::is_ascii_hexdigit));
    if printable && fits {
        Ok(())
    } else {
        Err(format!(
            "a passphrase is 8 to 63 printable characters, or 64 hex digits; this is {} bytes",
            psk.len()
        ))
    }
}

/// The `connect` command: the optional channel is the last field.
fn connect_line(ssid: &[u8], psk: &[u8], freq_mhz: Option<u16>) -> String {
    match freq_mhz {
        Some(freq) => format!("connect {} {} {freq}\n", hex(ssid), hex(psk)),
        None => format!("connect {} {}\n", hex(ssid), hex(psk)),
    }
}

/// Writes a command, waiting for the device to appear: the first few seconds of a boot.
fn command_when_there(radio: &dyn Radio, line: &str) -> Result<(), String> {
    let started = Instant::now();
    loop {
        match radio.write(line) {
            Err(err)
                if err.kind() == io::ErrorKind::NotFound && started.elapsed() < DEVICE_WAIT =>
            {
                sleep(Duration::from_millis(200));
            }
            other => return other.map_err(|err| format!("{DEVICE}: {err}")),
        }
    }
}

/// The channel the join ended up on, from the `freq` line the status has while connected. This program keeps it in its own
/// memory for the joins it still has to make; writing it down is `save`, which a person runs.
fn joined_freq(status: &str) -> Option<u16> {
    field(status, "freq")?.parse().ok().filter(|f| *f != 0)
}

/// One join: the command, then the wait for the port. Returns the name joined and the connected status.
fn join(
    radio: &dyn Radio,
    ssid: &[u8],
    psk: &[u8],
    freq_mhz: Option<u16>,
) -> Result<(Vec<u8>, String), String> {
    command_when_there(radio, &connect_line(ssid, psk, freq_mhz))?;
    radio.tell(&format!(
        "{:.1} s: connect sent for {}{}",
        since_start(),
        String::from_utf8_lossy(ssid),
        freq_mhz.map_or(String::new(), |f| format!(" on {f} MHz"))
    ));
    sleep(Duration::from_millis(300));
    let status = wait_for(radio, "the join", &["connected"], Duration::from_secs(300))?;
    // The channel of the BSS joined, which is what the kernel measured rather than what was asked for.
    if let Some(freq) = joined_freq(&status) {
        radio.tell(&format!("{:.1} s: joined on {freq} MHz", since_start()));
    }
    Ok((ssid.to_vec(), status))
}

/// Joins, and returns the connected status. With a saved name the command goes out at once: the kernel holds it until the
/// firmware is up and then joins from what it has heard, or on the channel `freq_mhz` says, with no sweep of the bands first. A
/// name that is saved is never swapped for another network that happened to answer; `find_network` runs only for credentials
/// that save no name at all, and then the channel is of no use and is not asked for.
fn connect(
    radio: &dyn Radio,
    creds: &Credentials,
    freq_mhz: Option<u16>,
) -> Result<(Vec<u8>, String), String> {
    match &creds.ssid {
        Some(ssid) => join(radio, ssid, &creds.psk, freq_mhz),
        None => {
            let ssid = find_network(radio)?;
            join(radio, &ssid, &creds.psk, None)
        }
    }
}

/// How long to wait after the `attempt`th failed join (1 for the first): 2, 4, 8, 16 s and then 30 s. The waits of
/// wpa_supplicant's own retry, capped.
fn backoff(attempt: u32) -> Duration {
    Duration::from_secs((1u64 << attempt.clamp(1, 5)).min(30))
}

/// Whether the driver has given up (`state stopped`): only a restart of the machine helps then, and waiting would be for
/// nothing.
fn stopped(radio: &dyn Radio) -> bool {
    radio
        .read()
        .is_ok_and(|text| field(&text, "state") == Some("stopped"))
}

/// One saved network, retried within the bounds. `pause` is what waits between the attempts, so that a test takes no seconds
/// at all. The channel is the one this boot has: what was saved to start with, dropped once a join showed the network is not
/// there (a directed scan of a channel that is not the network's hears nothing, and the next attempt must sweep).
fn join_one(
    radio: &dyn Radio,
    creds: &Credentials,
    started: Instant,
    pause: &dyn Fn(Duration),
) -> Result<(Vec<u8>, String), String> {
    let mut attempt = 0u32;
    let mut freq = creds.freq_mhz;
    loop {
        let why = match connect(radio, creds, freq) {
            Ok(joined) => return Ok(joined),
            Err(why) => why,
        };
        attempt += 1;
        if stopped(radio) {
            return Err(format!("{why}: the driver has stopped"));
        }
        if why == "connect: NoSuchNetwork" && freq.take().is_some() {
            radio.tell(&format!(
                "{:.1} s: attempt {attempt} is not on the channel it asked for; the next one sweeps",
                since_start()
            ));
        }
        if attempt >= MAX_ATTEMPTS {
            radio.tell(&format!(
                "{:.1} s: giving up on {} after {MAX_ATTEMPTS} attempts: {why}",
                since_start(),
                String::from_utf8_lossy(creds.ssid.as_deref().unwrap_or(b"the network picked")),
            ));
            return Err(why);
        }
        if started.elapsed() >= MAX_TOTAL {
            radio.tell(&format!(
                "{:.1} s: giving up on {} after {} s: {why}",
                since_start(),
                String::from_utf8_lossy(creds.ssid.as_deref().unwrap_or(b"the network picked")),
                MAX_TOTAL.as_secs(),
            ));
            return Err(why);
        }
        let wait = backoff(attempt);
        radio.tell(&format!(
            "{:.1} s: attempt {attempt} did not join: {why}; again in {} s",
            since_start(),
            wait.as_secs()
        ));
        pause(wait);
        if attempt >= 2 {
            // From the second retry on the results are suspect: a sweep comes first, and the next attempt joins from it.
            radio.write("scan\n").ok();
            sleep(Duration::from_millis(300));
            let _ = wait_for(
                radio,
                "the scan before the next attempt",
                &["idle", "connected"],
                Duration::from_secs(60),
            );
        }
    }
}

/// The boot's join: the saved networks in the order they are known, each tried within the bounds, and none of them swapped for
/// a network that merely answered. `next` gives the next one and `None` only when every source is spent, so that a source is
/// read just when a join has needed it; a name already tried is not tried again. This never writes anything.
/// A boot that finds nothing saved anywhere is an error and says so once: `lucina.wifi=on` asked for the network, and a machine
/// with no credentials is better off saying that in the log and ending with a failure than ending quietly with Wi-Fi off and no
/// reason given.
fn join_saved(
    radio: &dyn Radio,
    next: &mut dyn FnMut() -> Result<Option<Credentials>, String>,
    pause: &dyn Fn(Duration),
) -> Result<(Vec<u8>, String), String> {
    let started = Instant::now();
    let mut tried: Vec<Vec<u8>> = Vec::new();
    let mut last: Option<String> = None;
    while started.elapsed() < MAX_TOTAL {
        let Some(creds) = next()? else {
            return match last {
                Some(why) => Err(why),
                None => {
                    radio.tell(
                        "nothing saved anywhere: neither the initramfs nor the disk has credentials",
                    );
                    Err("no saved credentials".to_string())
                }
            };
        };
        let name = creds.ssid.clone().unwrap_or_default();
        if !name.is_empty() && tried.contains(&name) {
            continue;
        }
        tried.push(name.clone());
        match join_one(radio, &creds, started, pause) {
            Ok(joined) => return Ok(joined),
            Err(why) => {
                radio.tell(&format!(
                    "{:.1} s: {why}{}; the next saved network",
                    since_start(),
                    if name.is_empty() {
                        String::new()
                    } else {
                        format!(" for {}", String::from_utf8_lossy(&name))
                    }
                ));
                last = Some(why);
            }
        }
    }
    Err(match last {
        Some(why) => format!("{why}, and no time for another network"),
        None => format!("no join in {} s", MAX_TOTAL.as_secs()),
    })
}

/// The saved credentials on the disk, waiting for the disk to appear. The disk may never (a kernel without UFS), which is not
/// an error: there is then nothing to connect with. Reading only; the store is written by `save`.
fn store_credentials() -> Result<Option<Credentials>, String> {
    let started = Instant::now();
    loop {
        match esp::read_saved() {
            Ok(saved) => {
                let creds = saved
                    .map(|text| Credentials::parse(&String::from_utf8_lossy(&text)))
                    .transpose()?;
                if creds.is_some() {
                    tell("using the saved credentials");
                }
                return Ok(creds);
            }
            Err(_) if started.elapsed() < DISK_WAIT => sleep(Duration::from_secs(1)),
            Err(err) => {
                tell(&format!("no saved credentials to read: {err}"));
                return Ok(None);
            }
        }
    }
}

/// The saved credentials in the order they are known: the initramfs's file, then the disk's store. A source that is not there
/// is gone through, not a stop: `scripts/remote-initramfs.sh` writes `wifi.conf` only when the image was built with
/// `GBGO_WIFI_PSK`, so a machine whose credentials came from `lucina-wifi save` holds them in the store alone and must still
/// reach it. `None` therefore says both are spent, and the store is asked only once, as reading it waits for the disk and
/// mounts a partition. A file that is there but holds no passphrase is an error of its own and is not gone through: it was put
/// there wrong, and the disk's copy is a different network's secret.
fn saved_sources(
    conf: Option<String>,
    mut store: impl FnMut() -> Result<Option<Credentials>, String>,
) -> impl FnMut() -> Result<Option<Credentials>, String> {
    let mut conf = conf;
    let mut store_left = true;
    move || {
        if let Some(text) = conf.take() {
            return Credentials::parse(&text).map(Some);
        }
        if !store_left {
            return Ok(None);
        }
        store_left = false;
        store()
    }
}

/// At boot: the initramfs's credentials, and then the disk's if that did not join. Nothing here writes the store, and nothing
/// writes a channel: `connect --saved` leaves every file exactly as it found it.
fn connect_saved() -> Result<(), String> {
    let mut next = saved_sources(fs::read_to_string(CONF).ok(), store_credentials);
    let (_, status) = join_saved(&Device, &mut next, &|wait| sleep(wait))?;
    print!("{}", readable(&status));
    Ok(())
}

fn run(args: &[String]) -> Result<(), String> {
    let device = |err: io::Error| format!("{DEVICE}: {err}");
    match args.first().map(String::as_str) {
        None | Some("status") => print!("{}", readable(&status().map_err(device)?)),
        Some("scan") => {
            command("scan\n").map_err(device)?;
            sleep(Duration::from_millis(300));
            print!(
                "{}",
                readable(&wait_for(
                    &Device,
                    "the scan",
                    &["idle", "connected"],
                    Duration::from_secs(30)
                )?)
            );
        }
        Some("connect") => {
            if args.get(1).is_some_and(|a| a == "--saved") {
                return connect_saved();
            }
            let text = match args.get(1).map(String::as_str) {
                Some("--stdin") => {
                    let mut text = String::new();
                    io::stdin()
                        .read_to_string(&mut text)
                        .map_err(|err| err.to_string())?;
                    text
                }
                Some(path) => fs::read_to_string(path).map_err(|err| format!("{path}: {err}"))?,
                None => fs::read_to_string(CONF).map_err(|err| format!("{CONF}: {err}"))?,
            };
            // A person watching this waits for one answer, not for the boot's retries: `connect --saved` is the path that
            // retries, because nobody is there to see it fail.
            let creds = Credentials::parse(&text)?;
            let (_, status) = connect(&Device, &creds, creds.freq_mhz)?;
            print!("{}", readable(&status));
        }
        Some("disconnect") => command("disconnect\n").map_err(device)?,
        Some("save") => {
            if args.get(1).is_none_or(|a| a != "--stdin") {
                return Err("save takes its credentials from stdin: save --stdin".to_string());
            }
            let mut text = String::new();
            io::stdin()
                .read_to_string(&mut text)
                .map_err(|err| err.to_string())?;
            let creds = Credentials::parse(&text)?;
            check_psk(&creds.psk)?;
            esp::save(&creds.to_bytes())?;
            tell("the credentials are saved: the next boot with lucina.wifi=on connects by itself");
        }
        Some("saved") => match esp::read_saved()? {
            Some(saved) => match Credentials::parse(&String::from_utf8_lossy(&saved))?.ssid {
                Some(ssid) => println!("saved, for {}", String::from_utf8_lossy(&ssid)),
                None => println!("saved, for the strongest FRITZ!Box"),
            },
            None => println!("nothing saved"),
        },
        Some("forget") => {
            if esp::forget()? {
                tell("the saved credentials are gone");
            } else {
                println!("nothing saved");
            }
        }
        Some("migrate") => {
            if esp::migrate()? {
                tell("the credentials moved from the EFI partition to /data");
            } else {
                println!("nothing saved on the EFI partition");
            }
        }
        Some(other) => {
            return Err(format!(
                "unknown command {other}: status, scan, connect, disconnect, save, saved, forget, migrate"
            ));
        }
    }
    Ok(())
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            // The program runs in the background at boot: its errors belong in the kernel log, where the rest of its time line
            // is. The name of the network is safe to say there; the passphrase is not, and never reaches this text.
            tell(&format!("ended with an error: {err}"));
            eprintln!("lucina-wifi: {err}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bss(name: &str, dbm: i32) -> String {
        format!(
            "bss 112233445566 2412 {dbm} wpa2-psk {}\n",
            hex(name.as_bytes())
        )
    }

    #[test]
    fn hex_encodes_the_ssid_and_the_psk() {
        assert_eq!(hex(b"abc"), "616263");
        assert_eq!(hex(&[0x00, 0xff]), "00ff");
        assert_eq!(unhex("616263"), b"abc");
    }

    #[test]
    fn binary_ssids_survive_the_decode() {
        let raw = [0x01u8, 0x02, 0xff];
        let line = format!("bss aabbccddeeff 2412 -50 wpa2-psk {}\n", hex(&raw));
        // The name is not UTF-8, so `readable` keeps it hex rather than replacing bytes.
        let shown = readable(&line);
        assert!(shown.contains("hex:0102ff"), "{shown}");
        // `readable` gives the row without its `bss` key, and the name last.
        assert!(shown.starts_with("aabbccddeeff 2412 -50 wpa2-psk  hex:0102ff"));
        assert_eq!(unhex("0102ff"), raw.to_vec());
    }

    #[test]
    fn the_strongest_fritz_box_wins_and_others_do_not_count() {
        let status = format!(
            "state idle\n{}{}{}",
            bss("FRITZ!Box 7590 XY", -70),
            bss("Neighbour", -30),
            bss("FRITZ!Box 7590 XY", -50)
        );
        let (name, dbm, matched) = pick_fritz(&status).unwrap();
        assert_eq!(name, b"FRITZ!Box 7590 XY");
        assert_eq!((dbm, matched), (-50, 2));
    }

    #[test]
    fn no_fritz_box_is_none() {
        assert!(pick_fritz(&format!("state idle\n{}", bss("Neighbour", -30))).is_none());
        assert!(pick_fritz("state idle\nbss broken\n").is_none());
    }

    #[test]
    fn a_passphrase_is_checked_before_it_is_kept() {
        assert!(check_psk(b"long enough").is_ok());
        assert!(check_psk(b"short").is_err());
        assert!(check_psk(&[b'a'; 64]).is_ok());
        assert!(check_psk(&[b'g'; 64]).is_err());
        assert!(check_psk(&[b'a'; 65]).is_err());
        assert!(check_psk(b"has\x01a control").is_err());
        assert!(
            !check_psk(b"has\x01a control")
                .unwrap_err()
                .contains("control")
        );
    }

    #[test]
    fn the_credentials_are_one_file_and_not_a_pair_of_values() {
        let creds =
            Credentials::parse("ssid=test-net-tool\npsk=fake-pass-tool\nfreq=5260\n").unwrap();
        assert_eq!(creds.ssid.unwrap(), b"test-net-tool");
        assert_eq!(creds.psk, b"fake-pass-tool");
        assert_eq!(creds.freq_mhz, Some(5260));
        assert!(Credentials::parse("ssid=test-net-tool\n").is_err());
    }

    #[test]
    fn a_kernel_log_record_is_one_string_with_one_newline() {
        assert_eq!(kmsg_record("taking X"), "lucina-wifi: taking X\n");
        assert_eq!(kmsg_record("a\nb"), "lucina-wifi: a\nb\n");
    }

    #[test]
    fn the_connect_line_has_the_channel_last_when_there_is_one() {
        assert_eq!(connect_line(b"ab", b"pw", None), "connect 6162 7077\n");
        assert_eq!(
            connect_line(b"ab", b"pw", Some(5260)),
            "connect 6162 7077 5260\n"
        );
    }

    #[test]
    fn the_channel_comes_from_the_connected_status() {
        assert_eq!(
            joined_freq("state connected\nssid 6162\nfreq 5260\n"),
            Some(5260)
        );
        assert_eq!(joined_freq("state connected\n"), None);
        assert_eq!(joined_freq("state connected\nfreq 0\n"), None);
    }

    #[test]
    fn a_line_says_how_many_networks_the_table_holds() {
        assert_eq!(
            networks(&format!("state idle\n{}{}", bss("a", -40), bss("b", -50))),
            2
        );
        assert_eq!(networks("state scanning\nmac 02:00\n"), 0);
    }

    /// A radio that hears the given networks (name, MHz), joins a name it hears on the channel it hears it on, and remembers
    /// every line written to it and every line said about it.
    struct Model {
        heard: Vec<(&'static str, u16)>,
        status: std::cell::RefCell<String>,
        lines: std::cell::RefCell<Vec<String>>,
        said: std::cell::RefCell<Vec<String>>,
        /// Joins that end in a failure of the port before one works: an authentication that does not come.
        fail_joins: std::cell::Cell<u32>,
        /// The driver has stopped: whatever is written, the status says so.
        stopped: std::cell::Cell<bool>,
    }

    impl Model {
        fn new(heard: Vec<(&'static str, u16)>) -> Self {
            Self {
                status: std::cell::RefCell::new(format!("state idle\n{}", Self::table(&heard))),
                heard,
                lines: Default::default(),
                said: Default::default(),
                fail_joins: Default::default(),
                stopped: Default::default(),
            }
        }

        fn table(heard: &[(&str, u16)]) -> String {
            heard
                .iter()
                .map(|(name, freq)| {
                    format!(
                        "bss 112233445566 {freq} -40 wpa2-psk {}\n",
                        hex(name.as_bytes())
                    )
                })
                .collect()
        }

        fn said(&self) -> Vec<String> {
            self.said.borrow().clone()
        }

        /// The lines that were joins, in the order they went out.
        fn joins(&self) -> Vec<String> {
            self.lines
                .borrow()
                .iter()
                .filter(|l| l.starts_with("connect "))
                .cloned()
                .collect()
        }

        fn credentials(ssid: &[u8], psk: &[u8], freq_mhz: Option<u16>) -> Credentials {
            Credentials {
                ssid: Some(ssid.to_vec()),
                psk: psk.to_vec(),
                freq_mhz,
            }
        }

        /// A join that fails `n` times before it works.
        fn failing(self, n: u32) -> Self {
            self.fail_joins.set(n);
            self
        }
    }

    impl Radio for Model {
        fn write(&self, line: &str) -> io::Result<()> {
            self.lines.borrow_mut().push(line.to_string());
            let table = Self::table(&self.heard);
            let Some(rest) = line.strip_prefix("connect ") else {
                return Ok(());
            };
            let mut words = rest.split(' ');
            let wanted = words.next().unwrap_or("");
            let _secret = words.next();
            // A channel asked for that is not the one the network is on hears nothing, as a directed scan does.
            let asked = words.next().and_then(|f| f.trim().parse::<u16>().ok());
            let heard = self
                .heard
                .iter()
                .find(|(name, _)| hex(name.as_bytes()) == wanted)
                .filter(|(_, freq)| asked.is_none_or(|f| f == *freq));
            let failing = self.fail_joins.get() > 0;
            self.fail_joins.set(self.fail_joins.get().saturating_sub(1));
            *self.status.borrow_mut() = match heard {
                Some(_) if failing => format!("state idle\nfailed the join failed\n{table}"),
                Some((_, freq)) => {
                    format!("state connected\nssid {wanted}\nfreq {freq}\n{table}")
                }
                None => format!("state idle\nfailed connect: NoSuchNetwork\n{table}"),
            };
            Ok(())
        }

        fn read(&self) -> io::Result<String> {
            if self.stopped.get() {
                return Ok("state stopped\nfailed the driver stopped\n".to_string());
            }
            Ok(self.status.borrow().clone())
        }

        fn tell(&self, line: &str) {
            self.said.borrow_mut().push(line.to_string());
        }
    }

    /// The joins a saved network goes through, with no waiting and no bounds but the program's own.
    fn join_saved(radio: &dyn Radio, creds: &[Credentials]) -> Result<(Vec<u8>, String), String> {
        let mut left = creds.iter().cloned();
        super::join_saved(radio, &mut || Ok(left.next()), &|_| {})
    }

    /// The disk's store as a boot meets it: it holds one network and says how many times it was asked.
    struct Store {
        creds: std::cell::RefCell<Option<Credentials>>,
        asked: std::cell::Cell<u32>,
    }

    impl Store {
        fn of(ssid: &[u8], psk: &[u8], freq_mhz: Option<u16>) -> Self {
            Self {
                creds: std::cell::RefCell::new(Some(Model::credentials(ssid, psk, freq_mhz))),
                asked: Default::default(),
            }
        }

        fn read(&self) -> Result<Option<Credentials>, String> {
            self.asked.set(self.asked.get() + 1);
            Ok(self.creds.borrow_mut().take())
        }

        fn times_asked(&self) -> u32 {
            self.asked.get()
        }
    }

    /// A boot with no initramfs file and one network on the disk: the disk is reached, and its network is the one joined.
    /// This is the path `remote.sh` runs on a machine saved with `lucina-wifi save`, and closure `1 => Ok(None)` used to end
    /// the list before it.
    #[test]
    fn a_machine_saved_on_the_disk_alone_is_joined_from_the_disk() {
        let radio = Model::new(vec![("test-net-disk", 2437)]);
        let store = Store::of(b"test-net-disk", b"fake-pass-disk", Some(2437));
        let mut next = super::saved_sources(None, || store.read());
        let (joined, status) = super::join_saved(&radio, &mut next, &|_| {}).unwrap();
        assert_eq!(joined, b"test-net-disk");
        assert_eq!(joined_freq(&status), Some(2437));
        assert_eq!(store.times_asked(), 1);
        assert_eq!(
            radio.joins()[..],
            [connect_line(
                b"test-net-disk",
                b"fake-pass-disk",
                Some(2437)
            )]
        );
    }

    #[test]
    fn a_boot_with_no_credentials_anywhere_says_so_and_fails() {
        let radio = Model::new(vec![("test-net-nowhere", 2437)]);
        let store = Store {
            creds: Default::default(),
            asked: Default::default(),
        };
        let mut next = super::saved_sources(None, || store.read());
        let err = super::join_saved(&radio, &mut next, &|_| {}).unwrap_err();
        assert_eq!(err, "no saved credentials");
        assert_eq!(store.times_asked(), 1);
        assert!(radio.joins().is_empty());
        assert!(
            radio
                .said()
                .iter()
                .any(|l| l.contains("nothing saved anywhere")),
            "{:?}",
            radio.said()
        );
    }

    #[test]
    fn the_initramfs_file_is_joined_and_the_disk_is_never_read() {
        let radio = Model::new(vec![("test-net-file", 5260)]);
        let store = Store::of(b"test-net-disk-later", b"fake-pass-later", None);
        let mut next = super::saved_sources(
            Some("ssid=test-net-file\npsk=fake-pass-fileonly\nfreq=5260\n".to_string()),
            || store.read(),
        );
        let (joined, _) = super::join_saved(&radio, &mut next, &|_| {}).unwrap();
        assert_eq!(joined, b"test-net-file");
        assert_eq!(store.times_asked(), 0);
    }

    #[test]
    fn the_disk_is_the_network_after_the_initramfs_one_did_not_join() {
        let radio = Model::new(vec![("test-net-disknext", 2412)]);
        let store = Store::of(b"test-net-disknext", b"fake-pass-disknext", Some(2412));
        let mut next = super::saved_sources(
            Some("ssid=test-net-not-here\npsk=fake-pass-nhere\n".to_string()),
            || store.read(),
        );
        let (joined, _) = super::join_saved(&radio, &mut next, &|_| {}).unwrap();
        assert_eq!(joined, b"test-net-disknext");
        assert_eq!(store.times_asked(), 1);
        let joins = radio.joins();
        assert_eq!(joins.len(), MAX_ATTEMPTS as usize + 1, "{joins:?}");
        assert!(
            joins[..MAX_ATTEMPTS as usize]
                .iter()
                .all(|l| l.starts_with(&format!("connect {} ", hex(b"test-net-not-here"))))
        );
    }

    #[test]
    fn a_file_that_is_there_without_a_passphrase_is_said_and_the_disk_is_not_blamed_for_it() {
        let radio = Model::new(vec![("test-net-blank", 2437)]);
        let store = Store::of(b"test-net-blank", b"fake-pass-blank", None);
        let mut next =
            super::saved_sources(Some("ssid=test-net-blank\n".to_string()), || store.read());
        let err = super::join_saved(&radio, &mut next, &|_| {}).unwrap_err();
        assert_eq!(err, "the credentials need a psk= line");
        assert!(radio.joins().is_empty());
        assert_eq!(store.times_asked(), 0);
    }

    #[test]
    fn the_disk_is_asked_once_even_when_its_network_does_not_join() {
        let radio = Model::new(vec![("test-net-listening", 2437)]);
        let store = Store::of(b"test-net-quiet", b"fake-pass-quiet", None);
        let mut next = super::saved_sources(None, || store.read());
        let err = super::join_saved(&radio, &mut next, &|_| {}).unwrap_err();
        assert_eq!(err, "connect: NoSuchNetwork");
        assert_eq!(store.times_asked(), 1);
        assert_eq!(radio.joins().len(), MAX_ATTEMPTS as usize);
    }

    #[test]
    fn a_saved_name_goes_out_at_once_with_its_channel_and_without_a_scan() {
        let radio = Model::new(vec![("test-net-atonce", 5260)]);
        let creds = Model::credentials(b"test-net-atonce", b"fake-pass-atonce", Some(5260));
        let (joined, status) = super::connect(&radio, &creds, creds.freq_mhz).unwrap();
        assert_eq!(joined, b"test-net-atonce");
        assert_eq!(
            radio.lines.borrow()[..],
            [connect_line(
                b"test-net-atonce",
                b"fake-pass-atonce",
                Some(5260)
            )]
        );
        assert_eq!(joined_freq(&status), Some(5260));
    }

    #[test]
    fn a_name_is_taken_from_the_kernels_first_sweep_without_asking_for_a_scan() {
        let radio = Model::new(vec![("test-net-other", 2412), ("FRITZ!Box 7590 XY", 5260)]);
        let creds = Credentials {
            ssid: None,
            psk: b"fake-pass-sweep".to_vec(),
            freq_mhz: None,
        };
        let (joined, _) = super::connect(&radio, &creds, None).unwrap();
        assert_eq!(joined, b"FRITZ!Box 7590 XY");
        assert_eq!(
            radio.lines.borrow()[..],
            [connect_line(b"FRITZ!Box 7590 XY", b"fake-pass-sweep", None)]
        );
    }

    #[test]
    fn only_a_sweep_that_heard_nothing_leads_to_a_scan_of_our_own() {
        let radio = Model::new(vec![("test-net-none-here", 2412)]);
        let creds = Credentials {
            ssid: None,
            psk: b"fake-pass-scan".to_vec(),
            freq_mhz: None,
        };
        let err = super::connect(&radio, &creds, None).unwrap_err();
        assert_eq!(err, "no FRITZ!Box network");
        assert_eq!(*radio.lines.borrow(), ["scan\n"]);
    }

    #[test]
    fn the_time_line_says_what_it_waits_for_and_what_it_got() {
        let radio = Model::new(vec![("test-net-timeline", 2437)]);
        let creds = Model::credentials(b"test-net-timeline", b"fake-pass-timeline", None);
        super::connect(&radio, &creds, None).unwrap();
        let said = radio.said();
        assert!(
            said.iter()
                .any(|l| l.contains("waiting for the join (connected)")),
            "{said:?}"
        );
        assert!(
            said.iter()
                .any(|l| l.contains("the join: state connected after") && l.contains("1 networks")),
            "{said:?}"
        );
        assert!(
            said.iter().any(|l| l.contains("joined on 2437 MHz")),
            "{said:?}"
        );
    }

    #[test]
    fn the_waits_between_the_attempts_are_the_wpa_waits_and_capped() {
        let secs: Vec<u64> = (1..=7).map(|n| backoff(n).as_secs()).collect();
        assert_eq!(secs, [2, 4, 8, 16, 30, 30, 30]);
    }

    #[test]
    fn a_join_that_never_comes_gives_up_at_the_bound_and_says_so_once() {
        let radio = Model::new(vec![("test-net-bound", 2437)]).failing(MAX_ATTEMPTS + 3);
        let creds = Model::credentials(b"test-net-bound", b"fake-pass-bound", None);
        let err = join_saved(&radio, &[creds]).unwrap_err();
        assert_eq!(err, "the join failed");
        assert_eq!(radio.joins().len(), MAX_ATTEMPTS as usize);
        let said = radio.said();
        let given_up: Vec<&String> = said
            .iter()
            .filter(|l| l.contains("giving up on test-net-bound"))
            .collect();
        assert_eq!(given_up.len(), 1, "{said:?}");
        assert!(given_up[0].contains(&format!("{MAX_ATTEMPTS} attempts")));
    }

    #[test]
    fn a_join_outside_the_total_time_gives_up_without_another_attempt() {
        let radio = Model::new(vec![("test-net-time", 2437)]).failing(9);
        let creds = Model::credentials(b"test-net-time", b"fake-pass-time", None);
        let started = Instant::now() - MAX_TOTAL;
        let err = super::join_one(&radio, &creds, started, &|_| {}).unwrap_err();
        assert_eq!(err, "the join failed");
        assert_eq!(radio.joins().len(), 1);
        assert!(
            radio
                .said()
                .iter()
                .any(|l| l.contains(&format!("after {} s", MAX_TOTAL.as_secs()))),
            "{:?}",
            radio.said()
        );
    }

    #[test]
    fn a_channel_that_is_wrong_is_asked_for_once_and_then_left_out() {
        let radio = Model::new(vec![("test-net-stale", 5260)]);
        let creds = Model::credentials(b"test-net-stale", b"fake-pass-stale", Some(2412));
        let (_, status) = join_saved(&radio, &[creds]).unwrap();
        assert_eq!(joined_freq(&status), Some(5260));
        let joins = radio.joins();
        assert_eq!(joins.len(), 2, "{joins:?}");
        assert_eq!(
            joins[0],
            connect_line(b"test-net-stale", b"fake-pass-stale", Some(2412))
        );
        assert_eq!(
            joins[1],
            connect_line(b"test-net-stale", b"fake-pass-stale", None)
        );
        assert!(
            radio
                .said()
                .iter()
                .any(|l| l.contains("the next one sweeps")),
            "{:?}",
            radio.said()
        );
    }

    #[test]
    fn a_stopped_driver_ends_the_retries_at_once() {
        let radio = Model::new(vec![("test-net-stopped", 2437)]);
        radio.stopped.set(true);
        let creds = Model::credentials(b"test-net-stopped", b"fake-pass-stopped", None);
        let err = join_saved(&radio, &[creds]).unwrap_err();
        assert!(err.contains("the driver has stopped"), "{err}");
        assert_eq!(radio.joins().len(), 1);
    }

    #[test]
    fn a_saved_name_that_is_not_there_is_never_swapped_for_one_that_is() {
        let radio = Model::new(vec![("test-net-listen", 2437), ("FRITZ!Box 7590 XY", 5260)]);
        let creds = Model::credentials(b"test-net-gone", b"fake-pass-gone", None);
        let err = join_saved(&radio, &[creds]).unwrap_err();
        assert_eq!(err, "connect: NoSuchNetwork");
        // Every command that went out asked for the name the store holds. The FRITZ!Box that answered is left alone: it is not
        // the saved network, and joining it would be joining a network nobody saved.
        let asked_for = format!(
            "connect {} {}",
            hex(b"test-net-gone"),
            hex(b"fake-pass-gone")
        );
        let joins = radio.joins();
        assert_eq!(joins.len(), MAX_ATTEMPTS as usize, "{joins:?}");
        assert!(joins.iter().all(|l| l.starts_with(&asked_for)), "{joins:?}");
        assert!(!joins.iter().any(|l| l.contains(&hex(b"FRITZ!Box 7590 XY"))));
        assert!(
            !radio.said().iter().any(|l| l.contains("FRITZ!Box 7590 XY")),
            "{:?}",
            radio.said()
        );
    }

    #[test]
    fn the_other_saved_network_is_joined_and_only_it() {
        let radio = Model::new(vec![("test-net-second", 2437), ("FRITZ!Box 7590 XY", 5260)]);
        let first = Model::credentials(b"test-net-first", b"fake-pass-first", None);
        let second = Model::credentials(b"test-net-second", b"fake-pass-second", None);
        let (joined, _) = join_saved(&radio, &[first, second]).unwrap();
        assert_eq!(joined, b"test-net-second");
        assert_eq!(
            radio.joins().first().unwrap(),
            &connect_line(b"test-net-first", b"fake-pass-first", None)
        );
        assert_eq!(
            radio.joins().last().unwrap(),
            &connect_line(b"test-net-second", b"fake-pass-second", None)
        );
        assert_eq!(radio.joins().len(), MAX_ATTEMPTS as usize + 1);
        assert!(
            !radio
                .joins()
                .iter()
                .any(|l| l.contains(&hex(b"FRITZ!Box 7590 XY")))
        );
    }

    #[test]
    fn a_network_saved_in_both_places_is_tried_once() {
        let radio = Model::new(vec![("test-net-twice", 2437)]).failing(MAX_ATTEMPTS + 2);
        let once = Model::credentials(b"test-net-twice", b"fake-pass-twice", Some(2437));
        let again = Model::credentials(b"test-net-twice", b"fake-pass-twice", None);
        let err = join_saved(&radio, &[once, again]).unwrap_err();
        assert!(err.contains("the join failed"), "{err}");
        assert_eq!(radio.joins().len(), MAX_ATTEMPTS as usize);
    }

    /// Rule d of the join path: the passphrase goes in one command and in nothing else. These are every line the boot's log
    /// would get, and every text that reaches `main`'s error line, while a saved network is retried to the bound.
    #[test]
    fn nothing_the_join_path_logs_carries_the_passphrase() {
        let fake = b"fake-pass-not-logged";
        let radio = Model::new(vec![("test-net-logged", 2437), ("FRITZ!Box 7590 XY", 5260)])
            .failing(MAX_ATTEMPTS + 2);
        let creds = Model::credentials(b"test-net-logged", fake, Some(2437));
        let err = join_saved(&radio, &[creds]).unwrap_err();
        let mut texts = radio.said();
        texts.extend(
            radio
                .lines
                .borrow()
                .iter()
                .filter(|l| !l.starts_with("connect "))
                .cloned(),
        );
        texts.push(err);
        assert!(texts.len() > 8, "{texts:?}");
        for text in &texts {
            for (form, shown) in secret_forms(fake) {
                assert!(
                    !text.contains(&shown),
                    "{text} carries the passphrase as {form}"
                );
            }
        }
    }

    /// Every shape a secret takes once it reaches text: as it is, as the decimal list a derived `Debug` prints for a `Vec<u8>`,
    /// and in hex. A test that refuses a secret in a text refuses it in all of these, since any one of them is a leak.
    fn secret_forms(secret: &[u8]) -> Vec<(&'static str, String)> {
        let decimal = |sep: &str| {
            secret
                .iter()
                .map(u8::to_string)
                .collect::<Vec<_>>()
                .join(sep)
        };
        let hex = |upper: bool| {
            secret
                .iter()
                .map(|byte| {
                    if upper {
                        format!("{byte:02X}")
                    } else {
                        format!("{byte:02x}")
                    }
                })
                .collect()
        };
        vec![
            (
                "the bytes as they are",
                String::from_utf8_lossy(secret).into_owned(),
            ),
            ("a list of decimals", decimal(", ")),
            ("a list of decimals run together", decimal(",")),
            ("hex", hex(false)),
            ("hex in capitals", hex(true)),
        ]
    }
}
