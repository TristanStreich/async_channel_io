use async_channel::{Receiver, Recv, Sender};

use futures::{
    io::{AsyncRead, AsyncWrite},
    FutureExt,
};
use std::{future::Future, io::Write, pin::Pin, task::Poll};

pub struct ChannelWriter(pub Sender<Vec<u8>>);

impl AsyncWrite for ChannelWriter {
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        let msg = Vec::from(buf);
        let amount_written = msg.len();
        // TODO: this is most certainly wrong. I should not make a new send future each time
        std::pin::pin!(self.0.send(msg))
            .poll(cx)
            .map_ok(|_| amount_written)
            .map_err(|send_err| std::io::Error::new(std::io::ErrorKind::Other, send_err))
    }

    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn poll_close(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }
}

pub struct ChannelReader {
    recv: Pin<Box<Receiver<Vec<u8>>>>,
    fut: Option<Pin<Box<Recv<'static, Vec<u8>>>>>,
}

enum ChannelReaderData {
    /// We have no data in buffer and we have not asked for any
    None,
    /// We have data to give but no body as asked for it yet.
    /// This will happen if we receive a value from pending but
    /// The buffer we write to is not long enough to receive it all
    Some(Vec<u8>),
    /// We have asked for data and are waiting for a response
    Pending(Pin<Box<Recv<'static, Vec<u8>>>>),
}

impl ChannelReader {
    pub fn new(recv: Receiver<Vec<u8>>) -> Self {
        Self {
            recv: Box::pin(recv),
            fut: None,
        }
    }
}

impl Unpin for ChannelReader {}

impl AsyncRead for ChannelReader {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        mut buf: &mut [u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        let mut fut = self.fut.take().unwrap_or_else(|| {
            // SAFETY: Lmao probably not
            // but in reality this holds a reference to self.recv so as long we don't
            // leak fut or recv this "'static" should be valid.
            // must make sure we have self.recv pinned so the memory reference is valid
            let fut: Recv<'static, Vec<u8>> = unsafe { std::mem::transmute(self.recv.recv()) };
            Box::pin(fut)
        });

        let recieved_bytes = match fut.poll_unpin(cx) {
            Poll::Pending => {
                self.fut = Some(fut);
                return Poll::Pending;
            }
            Poll::Ready(bytes) => bytes,
        };

        let res = match recieved_bytes {
            Ok(val) => {
                // TODO: what about if buf is shorter than received bytes?
                // we will need to store the bytes we have not yet read into self to read later
                buf.write(&val)
            }
            // Error can only mean channel is closed so we will return 0 bytes read.
            // 0 Bytes read means EOF
            Err(_) => Ok(0),

        };

        Poll::Ready(res)
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use futures::io::AsyncReadExt;
    use tokio::time;

    #[tokio::test]
    async fn test_reader_simple() {
        let (send, recv) = async_channel::unbounded();
        let mut async_reader = ChannelReader::new(recv);

        // Have some waiting in receiver
        send.send(String::from("hello").into()).await.unwrap();

        tokio::spawn(async move {
            // send some later
            time::sleep(time::Duration::from_millis(50)).await;
            send.send(String::from(" world").into()).await.unwrap();
        });
        let mut buf = vec![0; 100];

        async_reader.read(&mut buf).await;

        let mut read = String::from_utf8_lossy(&buf[..5]).to_string();

        assert_eq!("hello", read);
        
        async_reader.read(&mut buf).await;
        let read2 = String::from_utf8_lossy(&buf[..6]).to_string();
        read.push_str(&read2);
        assert_eq!("hello world", read);
    }


    #[tokio::test]
    async fn test_reader_exact() {
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

    #[tokio::test]
    async fn test_read_small_buf() {
        let (send, recv) = async_channel::unbounded();
        let mut async_reader = ChannelReader::new(recv);

        // Have some waiting in receiver
        send.send(String::from("Hello").into()).await.unwrap();

        tokio::spawn(async move {
            // send some later
            time::sleep(time::Duration::from_millis(50)).await;
            send.send(String::from(" World!").into()).await.unwrap();
        });

        // buffer is small so we will need to call read many times to get the full message
        let mut buf = vec![0; 2];
        let mut read = String::new();
        while read.len() < 12 {
            println!("{read}");
            async_reader.read_exact(&mut buf).await.unwrap();
            read.push_str(std::str::from_utf8(&buf).unwrap());
        }
        assert_eq!("Hello World!", read);
    }


}
