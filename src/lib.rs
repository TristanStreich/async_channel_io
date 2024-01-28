use async_channel::{Receiver, Recv, Send, Sender};

use futures::{
    io::{AsyncRead, AsyncWrite},
    FutureExt,
};
use std::{cmp::Ordering, io::Write, pin::Pin, task::Poll};

pub fn pipe() -> (ChannelWriter, ChannelReader) {
    let (sender, recv) = async_channel::unbounded();
    (ChannelWriter::new(sender), ChannelReader::new(recv))
}

pub struct ChannelWriter {
    channel: Pin<Box<Sender<Vec<u8>>>>,
    fut: Option<Pin<Box<Send<'static, Vec<u8>>>>>,
}

impl ChannelWriter {
    pub fn new(sender: Sender<Vec<u8>>) -> Self {
        Self {
            channel: Box::pin(sender),
            fut: None,
        }
    }
}

impl AsyncWrite for ChannelWriter {
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        log::trace!("Polling Write");
        if let Some(mut fut) = self.fut.take() {
            log::trace!("Previous Send Found");
            // If the future is still pending then we cannot accept more data
            let Poll::Ready(send_res) = fut.poll_unpin(cx) else {
                log::trace!("Previous Send Still pending");
                self.fut = Some(fut);
                return Poll::Pending;
            };
            if let Err(_e) = send_res {
                // poll ready 0 means the channel is closed
                log::trace!("Sender Error. Returning EOF");
                return Poll::Ready(Ok(0));
            }
        }
        // Here Either the future was completed or we had no future to begin with so we can accept the data
        let msg = Vec::from(buf);
        let amount_written = msg.len();
        let fut: Send<'static, Vec<u8>> = unsafe { std::mem::transmute(self.channel.send(msg)) };
        let mut fut = Box::pin(fut);

        log::trace!("Sent {amount_written} bytes");

        // if we cannot immediately send then we store the future for later
        if fut.poll_unpin(cx).is_pending() {
            self.fut = Some(fut);
        }

        Poll::Ready(Ok(amount_written))
    }

    fn poll_flush(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        log::trace!("Polling Flush");
        self.fut
            .as_mut()
            // if there is a future we need to poll it to try and keep writing
            .map(|fut| fut.poll_unpin(cx))
            // if there is no future then its okay there is nothing to flush
            .unwrap_or_else(|| Poll::Ready(Ok(())))
            // if the channel had an error we don't care. We count this as a flush
            .map(|_| Ok(()))
    }

    fn poll_close(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        log::trace!("Polling Close");
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
    /// We have data to give but nobody as asked for it yet.
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
        log::trace!("Read {amount_written} bytes");
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
                unreachable!("Somehow we read more data from the vector than the vector contains!")
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
        log::trace!("Polling read");
        let mut fut = match self.data.take() {
            ChannelReaderData::Some(data) => {
                // here we will read as much as we can to buffer and
                // then either set data to remaining data or none
                log::trace!("Data Already Here");
                return Poll::Ready(self.try_write(data, buf));
            }
            ChannelReaderData::None => {
                // Time to ask for more data from the channel

                // SAFETY: Lmao probably not
                // but in reality this holds a reference to self.recv so as long we don't
                // leak fut or recv this "'static" should be valid.
                // must make sure we have self.recv pinned so the memory reference is valid
                log::trace!("Making New Recv future");
                let fut: Recv<'static, Vec<u8>> = unsafe { std::mem::transmute(self.recv.recv()) };
                Box::pin(fut)
            }
            ChannelReaderData::Pending(fut) => fut,
        };

        let received_result = match fut.poll_unpin(cx) {
            Poll::Pending => {
                self.data = ChannelReaderData::Pending(fut);
                log::trace!("Receiver is still pending");
                return Poll::Pending;
            }
            Poll::Ready(bytes) => bytes,
        };

        let Ok(bytes) = received_result else {
            // Error can only mean channel is closed so we will return 0 bytes read.
            // 0 Bytes read means EOF
            log::trace!("Receiver Error. Returning EOF");
            return Poll::Ready(Ok(0));
        };

        log::trace!("Received {} bytes", bytes.len());

        Poll::Ready(self.try_write(bytes, buf))
    }
}

#[allow(clippy::unused_io_amount)]
#[cfg(test)]
mod test {
    use super::*;
    use futures::{AsyncReadExt, AsyncWriteExt};
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

        async_reader.read(&mut buf).await.unwrap();

        let mut read = String::from_utf8_lossy(&buf[..5]).to_string();

        assert_eq!("hello", read);

        async_reader.read(&mut buf).await.unwrap();
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
            async_reader.read_exact(&mut buf).await.unwrap();
            read.push_str(std::str::from_utf8(&buf).unwrap());
        }
        assert_eq!("Hello World!", read);
    }

    #[tokio::test]
    async fn test_write_simple() {
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
        }
        assert_eq!(message, "Hello World!")
    }


    #[tokio::test]
    async fn test_write_flush() {
        let (send, recv) = async_channel::unbounded();
        let mut async_writer = ChannelWriter::new(send);

        async_writer.write(b"Hello").await.unwrap();
        tokio::spawn(async move {
            time::sleep(time::Duration::from_millis(50)).await;
            async_writer.flush().await.unwrap();
            async_writer.flush().await.unwrap();
            async_writer.write(b" World!").await.unwrap();
            async_writer.flush().await.unwrap();
            async_writer.flush().await.unwrap();
            async_writer.flush().await.unwrap();
        });

        let mut message = String::new();

        while let Ok(received) = recv.recv().await {
            message.push_str(std::str::from_utf8(&received).unwrap())
        }
        assert_eq!(message, "Hello World!")
    }
}
