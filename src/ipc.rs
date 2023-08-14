use std::{
    collections::BTreeMap,
    io::{self, prelude::*, BufReader},
    ops::Range,
};

use crate::{esafeprintln, opts::Client, DumpRange, Item};
use anyhow::Context;
use interprocess::local_socket::{self, LocalSocketListener, LocalSocketStream};

const MSG_REQUEST: &str = "Request: ";
const MSG_STOP: &str = "stop\n";

pub fn get_address() -> String {
    // TODO: this could be lazy initialized
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

    let listener = match LocalSocketListener::bind(address.clone()) {
        Err(e) if e.kind() == io::ErrorKind::AddrInUse => {
            // TODO: cleanup and retry strategy
            esafeprintln!("Error: socket {} is already in use", address);
            std::process::exit(1);
        }
        x => x.expect("Unexpected Socket error"),
    };
    //dbg!("Server running", name);
    listener
}

// TODO: T might be better as an enum or Box<dyn>
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
        //dbg!("Incoming connection!");
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
    //dbg!("Stopping Server!");
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

    // dbg!("Client sent", &buffer);

    if buffer == MSG_STOP {
        return Ok(ServerDirective::Stop);
    }

    let index = buffer
        .trim()
        .split_once(MSG_REQUEST)
        .and_then(|(_, msg)| msg.parse::<usize>().ok())
        .context("Malformed Message. Expected:\nRequest: idx\n")?;

    let range = items.values().nth(index).cloned();

    let writer = conn.get_mut();

    // TODO: dump_range_into_writer should not panic
    dump_ctx
        .dump_range_into_writer(range, writer)
        .context("Unexpected Error while dumping")?;
    Ok(ServerDirective::Continue)
}

/// Connects to a server and requests a dump with specified index
/// and immediately prints it to stdout.
pub fn start_client(req: Client) {
    let mut buffer = Vec::with_capacity(128);

    // Blocks until server accepts connection
    let conn = LocalSocketStream::connect(req.server_name).expect("Failed to connect to server");
    let mut conn = BufReader::new(conn);

    writeln!(conn.get_mut(), "{MSG_REQUEST}{}", req.select).expect("Socket send failed");

    // TODO: send data in fixed size batches or try piping local socket directly to stdout
    conn.read_to_end(&mut buffer)
        .expect("Client receive failed");

    std::io::stdout()
        .write_all(&buffer)
        .expect("Write to stdout failed");
}

/// The server process itself connects to the socket and tells it to stop
///
/// Blocks until server accepts a connection
pub fn send_server_stop() {
    LocalSocketStream::connect(get_address())
        .and_then(|mut conn| conn.write_all(MSG_STOP.as_bytes()))
        .expect("Failed to send stop");
}
