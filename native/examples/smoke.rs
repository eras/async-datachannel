use std::sync::Arc;

use async_datachannel::{Message, PeerConnection, RtcConfig};
use async_tungstenite::{tokio::connect_async, tungstenite};
use futures::{
    channel::mpsc,
    io::{AsyncReadExt, AsyncWriteExt},
    SinkExt, StreamExt,
};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tracing::{debug, info};

/// Works with the signalling server from https://github.com/paullouisageneau/libdatachannel/tree/master/examples/signaling-server-rust
/// Start two shells
/// 1. RUST_LOG=debug cargo run --example smoke -- ws://127.0.0.1:8000 other_peer
/// 2. RUST_LOG=debug cargo run --example smoke -- ws://127.0.0.1:8000 initiator other_peer

#[derive(Debug, Serialize, Deserialize)]
struct SignalingMessage {
    // id of the peer this messaged is supposed for
    id: String,
    payload: Message,
}
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    let ice_servers = vec!["stun:stun.l.google.com:19302"];
    let conf = RtcConfig::new(&ice_servers);
    let (tx_sig_outbound, mut rx_sig_outbound) = mpsc::channel(32);
    let (mut tx_sig_inbound, rx_sig_inbound) = mpsc::channel(32);
    let listener = PeerConnection::new(&conf, (tx_sig_outbound, rx_sig_inbound))?;

    let mut input = std::env::args().skip(1);

    let signaling_uri = input.next().unwrap();
    let my_id = input.next().unwrap();
    let signaling_uri = format!("{}/{}", signaling_uri, my_id);
    let peer_to_dial = input.next();
    info!("Trying to connect to {}", signaling_uri);

    let (mut write, mut read) = connect_async(&signaling_uri).await?.0.split();

    let other_peer = Arc::new(Mutex::new(peer_to_dial.clone()));
    let other_peer_c = other_peer.clone();
    let f_write = async move {
        while let Some(m) = rx_sig_outbound.next().await {
            let m = SignalingMessage {
                payload: m,
                id: other_peer_c.lock().as_ref().cloned().unwrap(),
            };
            let s = serde_json::to_string(&m).unwrap();
            debug!("Sending {:?}", s);
            write.send(tungstenite::Message::text(s)).await.unwrap();
        }
        anyhow::Result::<_, anyhow::Error>::Ok(())
    };
    tokio::spawn(f_write);
    let f_read = async move {
        while let Some(Ok(m)) = read.next().await {
            debug!("received {:?}", m);
            if let Some(val) = match m {
                tungstenite::Message::Text(t) => {
                    Some(serde_json::from_str::<serde_json::Value>(&t).unwrap())
                }
                tungstenite::Message::Binary(b) => Some(serde_json::from_slice(&b[..]).unwrap()),
                tungstenite::Message::Close(_) => panic!(),
                _ => None,
            } {
                let c: SignalingMessage = serde_json::from_value(val).unwrap();
                println!("msg {:?}", c);
                other_peer.lock().replace(c.id);
                if tx_sig_inbound.send(c.payload).await.is_err() {
                    panic!()
                }
            }
        }
        anyhow::Result::<_, anyhow::Error>::Ok(())
    };

    tokio::spawn(f_read);

    let bytes_to_transfer = 500 * 1000 * 1000;
    let mut buf: [u8; 512] = [0; 512];

    let check = true;

    if peer_to_dial.is_some() {
        let mut dc = listener.dial("whatever").await?;
        info!("dial succeed");

        let mut frame = 0;
        let mut transferred = 0;
        let mut value: u8 = 0; // next value to write to the buffer
        while transferred < bytes_to_transfer {
            let write_bytes = std::cmp::min(bytes_to_transfer - transferred, buf.len());
            if check {
                for c in 0..write_bytes {
                    buf[c] = value;
                    value = (value + 1) % 251;
                }
            }
            dc.write_all(&buf[0..write_bytes]).await?;
            frame += 1;
            transferred += write_bytes;
            let remaining = bytes_to_transfer - transferred;
            println!("sent {write_bytes} bytes, remaining {remaining} at frame {frame}");
        }
        info!("waiting ack");
        let _n = dc.read(&mut buf[0..1]).await?;
        info!("done and done");
    } else {
        let mut dc = listener.accept().await?;
        info!("accept succeed");

        let mut frame = 0;
        let mut transferred = 0;
        let mut value: u8 = 0; // next value to expect from the buffer

        while transferred < bytes_to_transfer {
            let n = dc.read(&mut buf).await?;
            frame += 1;
            if check {
                for c in 0..n {
                    if buf[c] != value {
                        eprintln!(
                            "EXPECTED {value}, RECEIVED {buf} at {transferred}+{c} at frame {frame}",
                            buf = buf[c],
                        );
                        value = buf[c];
                    }
                    value = (value + 1) % 251;
                }
            }
            transferred += n;
            let remaining = bytes_to_transfer - transferred;
            println!("read {n} bytes, remaining {remaining} at frame {frame}");
        }
        println!("read all, sending ack");
        dc.write_all(&buf[0..1]).await?;
    };

    Ok(())
}
