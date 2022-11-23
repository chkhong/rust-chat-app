use std::collections::{HashMap, hash_map::Entry};
use std::sync::Arc;
use async_std::io::WriteExt;
use async_std::{task, net::{ToSocketAddrs, TcpListener, TcpStream}, io::{BufReader, prelude::BufReadExt}};
use futures::SinkExt;
use futures::select;
use futures::{StreamExt, Future, channel::mpsc};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

type Sender<T> = mpsc::UnboundedSender<T>;
type Receiver<T> = mpsc::UnboundedReceiver<T>;

#[derive(Debug)]
enum Void {}

pub(crate) fn main() -> Result<()> {
    task::block_on(accept_loop("127.0.0.1:8080"))
}

async fn accept_loop(addr: impl ToSocketAddrs) -> Result<()> {
    let listener = TcpListener::bind(addr).await?;

    let mut incoming = listener.incoming();
    
    let (broker_sender, broker_receiver) = mpsc::unbounded();
    let broker = task::spawn(spawn_and_log_error(broker_loop(broker_receiver)));

    while let Some(stream) = incoming.next().await {
        let stream = stream?;
        println!("Receiving from {}", stream.peer_addr()?);
        spawn_and_log_error(connection_loop(broker_sender.clone(), stream));
    }

    drop(broker_sender);
    broker.await;

    Ok(())
}

async fn connection_loop(mut broker: Sender<Event>, stream: TcpStream) -> Result<()> {
    let stream = Arc::new(stream);
    let reader = BufReader::new(&*stream);
    let mut lines = reader.lines();

    let (_shutdown_sender, shutdown_receiver) = mpsc::unbounded::<Void>();
    
    let name = match lines.next().await {
        Some(line) => line?,
        None => return Err("peer disconnected immediately".into())
    };

    broker.send(
        Event::NewPeer { 
            name: name.clone(),
            stream: Arc::clone(&stream),
            shutdown: shutdown_receiver
        })
        .await
        .unwrap();

    while let Some(line) = lines.next().await {
        let line = line?;

        let (dest, msg) = match line.find(':') {
            Some(idx) => (&line[..idx], line[idx+1..].trim()),
            None => continue
        };

        let dest: Vec<String> = dest.split(',').map(|name| name.trim().to_string()).collect();
        let msg = msg.to_string();

        broker.send(
            Event::Message {
                from: name.clone(),
                to: dest,
                msg
            })
            .await
            .unwrap()
    }

    Ok(())
}

async fn writer_loop(messages: &mut Receiver<String>, stream: Arc<TcpStream>, mut shutdown: Receiver<Void>) -> Result<()> {
    let mut stream = &*stream;
    loop {
        select! {
            msg = messages.next() => match msg {
                Some(msg) => stream.write_all(msg.as_bytes()).await?,
                None => break
            },
            void = shutdown.next() => match void {
                Some(void) => match void {},
                None => break
            }
        }
    }
    Ok(())
}

enum Event {
    NewPeer {
        name: String,
        stream: Arc<TcpStream>,
        shutdown: Receiver<Void>
    },
    Message {
        from: String,
        to: Vec<String>,
        msg: String
    }
}

async fn broker_loop(mut events: Receiver<Event>) -> Result<()> {
    let (disconnect_sender, mut disconnect_receiver) = mpsc::unbounded::<(String, Receiver<String>)>();
    let mut peers: HashMap<String, Sender<String>> = HashMap::new();

    loop {
        let event = select! {
            event = events.next() => match event {
                Some(event) => event,
                None => break
            },
            disconnect = disconnect_receiver.next() => {
                let (name, _pending_messages) = disconnect.unwrap();
                assert!(peers.remove(&name).is_some());
                continue;
            }
        };
        match event {
            Event::NewPeer { name, stream , shutdown} => {
                match peers.entry(name.clone()) {
                    Entry::Occupied(..) => (),
                    Entry::Vacant(entry) => {
                        let (client_sender, mut client_receiver) = mpsc::unbounded();
                        entry.insert(client_sender);
                        let mut disconnect_sender = disconnect_sender.clone();
                        spawn_and_log_error(async move {
                            let res = writer_loop(&mut client_receiver, stream, shutdown).await;
                            disconnect_sender.send((name, client_receiver)).await.unwrap();
                            res
                        });
                    }
                }
            },
            Event::Message { from, to, msg } => {
                for addr in to {
                    if let Some(peer) = peers.get_mut(&addr) {
                        let msg = format!("from {}: {}\n", from, msg);
                        peer.send(msg).await.unwrap();
                    }
                }
            },
        }
    }

    drop(peers);
    drop(disconnect_sender);
    while let Some((_name, _pending_messages)) = disconnect_receiver.next().await {}
    Ok(())
}


fn spawn_and_log_error<F>(fut: F) -> task::JoinHandle<()> 
where
    F: Future<Output = Result<()>> + Send + 'static
{
    task::spawn(async move {
        if let Err(e) = fut.await {
            eprintln!("{}", e)
        }
    })
}
