use async_channel::{Receiver, Recv, Sender};

use futures::{
    future::Pending,
    io::{AsyncRead, AsyncWrite},
    FutureExt,
};
use std::{cmp::Ordering, future::Future, io::Write, pin::Pin, task::Poll};

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
    data: ChannelReaderData,
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

impl ChannelReaderData {
    pub fn take(&mut self) -> Self {
        std::mem::replace(self, Self::None)
    }
}

impl ChannelReader {
    pub fn new(recv: Receiver<Vec<u8>>) -> Self {
        Self {
            recv: Box::pin(recv),
            data: ChannelReaderData::None,
        }
    }

    fn try_write(&mut self, mut to_write: Vec<u8>, mut buf: &mut [u8]) -> std::io::Result<usize> {
        let amount_written = buf.write(&to_write)?;
        match amount_written.cmp(&to_write.len()) {
            // We read everything from the vec into the buffer
            // We can now set self.data to None
            Ordering::Equal => self.data = ChannelReaderData::None,
            // We filled the buffer but did not read all of vec
            Ordering::Less => {
                // FIXME: this drain copies all elements. Might be a botleneck
                // consider vecdeque over vec for this impl
                // another option is to store a counter of amount written and slice &to_write with it (probably most performant?)
                to_write.drain(0..amount_written);
                self.data = ChannelReaderData::Some(to_write);
            }
            Ordering::Greater => {
                unreachable!("Somehow we read more data from the vector that the vector contains!")
            }
        }
        Ok(amount_written)
    }
}

impl Unpin for ChannelReader {}

impl AsyncRead for ChannelReader {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut [u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        let mut fut = match self.data.take() {
            ChannelReaderData::Some(data) => {
                // here we will read as much as we can to buffer and
                // then either set data to remaining data or none
                return Poll::Ready(self.try_write(data, buf));
            }
            ChannelReaderData::None => {
                // Time to ask for more data from the channel

                // SAFETY: Lmao probably not
                // but in reality this holds a reference to self.recv so as long we don't
                // leak fut or recv this "'static" should be valid.
                // must make sure we have self.recv pinned so the memory reference is valid
                let fut: Recv<'static, Vec<u8>> = unsafe { std::mem::transmute(self.recv.recv()) };
                Box::pin(fut)
            }
            ChannelReaderData::Pending(fut) => fut,
        };

        let received_result = match fut.poll_unpin(cx) {
            Poll::Pending => {
                self.data = ChannelReaderData::Pending(fut);
                return Poll::Pending;
            }
            Poll::Ready(bytes) => bytes,
        };

        let Ok(bytes) = received_result else {
            // Error can only mean channel is closed so we will return 0 bytes read.
            // 0 Bytes read means EOF
            return Poll::Ready(Ok(0));
        };

        Poll::Ready(self.try_write(bytes, buf))
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
