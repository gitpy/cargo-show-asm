//! Inter Process Communication (IPC) in a server/client architecture to provide
//! dumps of individual functions as a service.
//!
//!
//! The protocol currently allows for following messages:
//! - <code>Request: <i>index</i>\n</code>
//!
//!   Which sends the dump of the `nth` function cached in the servers Map.
//!   On server errors the response has the form <code>Error: <i>msg</i></code>.
//!   The end of the response is either a `EOF` or closing of the connection
//!
//!
//! - `Stop\n`
//!
//!   This message tells the server to shutdown and will not send a response

use std::{
    collections::BTreeMap,
    io::{self, prelude::*, stdout, BufReader},
    ops::Range,
};

use crate::{esafeprintln, opts::Client, DumpRange, Item};
use anyhow::{bail, Context};
use interprocess::local_socket::{self, LocalSocketListener, LocalSocketStream};

const MSG_REQUEST: &str = "Request: ";
const MSG_STOP: &str = "Stop\n";

pub fn get_address() -> String {
    use local_socket::NameTypeSupport;
    let pid = std::process::id();
    match NameTypeSupport::query() {
        NameTypeSupport::OnlyPaths => format!("/tmp/cargo_show_asm/server{pid}.sock"),
        NameTypeSupport::OnlyNamespaced | NameTypeSupport::Both => {
            format!("@cargo_show_asm.server{pid}.sock")
        }
    }
}

fn get_socket() -> LocalSocketListener {
    let address = get_address();

    match LocalSocketListener::bind(address.clone()) {
        Err(e) if e.kind() == io::ErrorKind::AddrInUse => {
            esafeprintln!("Error: socket {} is already in use", address);
            std::process::exit(1);
        }
        x => x.expect("Unexpected Socket error"),
    }
}

pub fn start_server<T>(items: &BTreeMap<Item, Range<usize>>, dump_ctx: &T)
where
    T: DumpRange + Send + Sync,
{
    fn socket_error(conn: io::Result<LocalSocketStream>) -> Option<LocalSocketStream> {
        match conn {
            Ok(c) => Some(c),
            Err(e) => {
                esafeprintln!("Incoming connection failed: {}", e);
                None
            }
        }
    }

    let listener = get_socket();
    let mut buffer = String::with_capacity(128);

    for conn in listener.incoming().filter_map(socket_error) {
        buffer.clear();

        let result = handle_request(conn, &mut buffer, items, dump_ctx);

        match result {
            Ok(ServerDirective::Continue) => continue,
            Ok(ServerDirective::Stop) => break,
            Err(e) => {
                esafeprintln!("{e}");
                continue;
            }
        }
    }
}

enum ServerDirective {
    Continue,
    Stop,
}

fn handle_request<T>(
    conn: LocalSocketStream,
    buffer: &mut String,
    items: &BTreeMap<Item, Range<usize>>,
    dump_ctx: &T,
) -> anyhow::Result<ServerDirective>
where
    T: DumpRange + Send + Sync,
{
    let mut conn = BufReader::new(conn);

    conn.read_line(buffer)
        .context("Failed to read from client")?;

    if buffer == MSG_STOP {
        return Ok(ServerDirective::Stop);
    }

    let writer = conn.get_mut();

    let index = buffer
        .trim()
        .split_once(MSG_REQUEST)
        .and_then(|(_, msg)| msg.parse::<usize>().ok())
        .with_context(|| {
            let msg = "Error: Malformed Message Expected:\nRequest: idx\n";
            let _ = writer.write_all(msg.as_bytes());
            msg
        })?;

    let range = items.values().nth(index);

    if range.is_none() {
        let msg = format!("Error: the requested index {index} is not found\n");
        writer.write_all(msg.as_bytes())?;
        bail!(msg)
    }

    dump_ctx
        .dump_range_into_writer(range.cloned(), writer)
        .context("Unexpected Error while dumping")?;
    Ok(ServerDirective::Continue)
}

/// Connects to a server and requests a dump with specified index
/// and immediately prints it to stdout.
pub fn start_client(req: Client) {

    // Blocks until server accepts connection
    let conn = LocalSocketStream::connect(req.server_name.as_str())
        .or_else(|_| {
            // When socket is not available yet retry in 100ms
            std::thread::sleep(std::time::Duration::from_millis(100));
            LocalSocketStream::connect(req.server_name.as_str())
        })
        .expect("Failed to connect to server");
    let mut conn = BufReader::new(conn);

    writeln!(conn.get_mut(), "{MSG_REQUEST}{}", req.select).expect("Connection failed on request");

    io::copy(&mut conn, &mut stdout()).expect("Pass-through of dump failed");
}

/// The server process itself connects to the socket and tells it to stop
///
/// Blocks until server accepts a connection
pub fn send_server_stop() {
    LocalSocketStream::connect(get_address())
        .and_then(|mut conn| conn.write_all(MSG_STOP.as_bytes()))
        .expect("Failed to send stop");
}

#[test]
fn ping_pong_test() {
    struct EchoDump<'a> {
        data: Vec<&'a str>,
    }
    impl DumpRange for EchoDump<'_> {
        fn dump_range_into_writer(
            &self,
            range: Option<Range<usize>>,
            writer: &mut impl Write,
        ) -> anyhow::Result<()> {
            let lines = range.map_or(self.data.as_slice(), |r| &self.data[r]);

            for line in lines {
                writeln!(writer, "{line}")?;
            }

            Ok(())
        }
    }

    let file = "First\n\
                Second\n\
                Third\n";

    let dump_ctx = EchoDump {
        data: file.lines().collect(),
    };
    let mut items = BTreeMap::new();
    items.insert(
        Item {
            name: "first()".to_string(),
            hashed: "first154232".to_string(),
            len: 0,
            index: 0,
        },
        0..1,
    );
    items.insert(
        Item {
            name: "second()".to_string(),
            hashed: "second63452".to_string(),
            len: 0,
            index: 1,
        },
        1..2,
    );
    items.insert(
        Item {
            name: "third()".to_string(),
            hashed: "third43534".to_string(),
            len: 0,
            index: 2,
        },
        2..3,
    );

    let clients = items
        .keys()
        .enumerate()
        .map(|(idx, _)| Client {
            client: (),
            server_name: get_address(),
            select: idx,
        })
        .collect::<Vec<Client>>();
    let server_handle = std::thread::spawn(move || start_server(&items, &dump_ctx));
    std::thread::scope(|s| {
        for client in clients {
            s.spawn(|| start_client(client));
        }
    });
    send_server_stop();
    server_handle.join().unwrap();
}
