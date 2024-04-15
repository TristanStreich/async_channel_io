# async_channel_io


[![Crates.io][crates-badge]][crates-url]
[![MIT licensed][mit-badge]][mit-url]
[![Build Status][actions-badge]][actions-url]

[crates-badge]: https://img.shields.io/crates/v/async_channel_io.svg
[crates-url]: https://crates.io/crates/async_channel_io
[mit-badge]: https://img.shields.io/badge/license-MIT-blue.svg
[mit-url]: https://github.com/TristanStreich/channel_io/blob/main/LICENSE
[actions-badge]: https://github.com/TristanStreich/channel_io/workflows/CI/badge.svg
[actions-url]: https://github.com/TristanStreich/channel_io/actions?query=branch%3Amain++

Wrappers around [`async_channel`] [`Sender`] and [`Receiver`] which implement [`AsyncRead`] and [`AsyncWrite`]

[`AsyncRead`]: https://docs.rs/futures/latest/futures/io/trait.AsyncRead.html
[`AsyncWrite`]: https://docs.rs/futures/latest/futures/io/trait.AsyncWrite.html
[`async_channel`]: https://docs.rs/async-channel/latest/async_channel/index.html
[`Sender`]: https://docs.rs/async-channel/latest/async_channel/struct.Sender.html
[`Receiver`]: https://docs.rs/async-channel/latest/async_channel/struct.Receiver.html

# Examples

### Writer

```rust
use tokio::time;
use channel_io::ChannelWriter;
use futures::AsyncWriteExt;

async fn example() {
    let (send, recv) = async_channel::unbounded();
    let mut async_writer = ChannelWriter::new(send);

    async_writer.write(b"Hello").await.unwrap();
    tokio::spawn(async move {
        time::sleep(time::Duration::from_millis(50)).await;
        async_writer.write(b" World!").await.unwrap();
    });

    let mut message = String::new();

    while let Ok(received) = recv.recv().await {
        message.push_str(std::str::from_utf8(&received).unwrap())
    };
    assert_eq!(message, "Hello World!")
}
```




### Reader
```rust
use tokio::time;
use channel_io::ChannelReader;
use futures::AsyncReadExt;

async fn example() {
    let (send, recv) = async_channel::unbounded();
    let mut async_reader = ChannelReader::new(recv);

    // Have some waiting in receiver
    send.send(String::from("hello").into()).await.unwrap();

    tokio::spawn(async move {
        // send some later
        time::sleep(time::Duration::from_millis(50)).await;
        send.send(String::from(" world").into()).await.unwrap();
    });
    let mut buf = vec![0; 11];

    async_reader.read_exact(&mut buf).await.unwrap();

    let read = String::from_utf8_lossy(&buf).to_string();
    assert_eq!("hello world", read);
}
```