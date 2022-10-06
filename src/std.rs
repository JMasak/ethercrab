use crate::{client::Client, timer_factory::TimerFactory};
use core::{future::Future, task::Poll};
use embassy_futures::select;
use pnet::datalink::{self, DataLinkReceiver, DataLinkSender};
use std::sync::Arc;

pub fn get_tx_rx(
    device: &str,
) -> Result<(Box<dyn DataLinkSender>, Box<dyn DataLinkReceiver>), std::io::Error> {
    let interfaces = datalink::interfaces();

    // dbg!(&interfaces);

    let interface = interfaces
        .into_iter()
        .find(|interface| interface.name == device)
        .expect("Could not find interface");

    // dbg!(interface.mac);

    let (tx, rx) = match datalink::channel(&interface, Default::default()) {
        Ok(datalink::Channel::Ethernet(tx, rx)) => (tx, rx),
        // FIXME
        Ok(_) => panic!("Unhandled channel type"),
        Err(e) => return Err(e),
    };

    Ok((tx, rx))
}

// TODO: Proper error - there are a couple of unwraps in here
// TODO: Make some sort of split() method to ensure we can only ever have one tx/rx future running
pub fn tx_rx_task<const MAX_FRAMES: usize, const MAX_PDU_DATA: usize, TIMEOUT>(
    device: &str,
    client: &Arc<Client<MAX_FRAMES, MAX_PDU_DATA, TIMEOUT>>,
) -> Result<impl Future<Output = embassy_futures::select::Either<(), ()>>, std::io::Error>
where
    TIMEOUT: TimerFactory + Send + 'static,
{
    let client_tx = client.clone();
    let client_rx = client.clone();

    let (mut tx, mut rx) = get_tx_rx(device)?;

    // TODO: Unwraps
    let tx_task = core::future::poll_fn::<(), _>(move |ctx| {
        client_tx
            .pdu_loop
            .send_frames_blocking(ctx.waker(), |frame, data| {
                let mut packet_buf = [0u8; 1536];

                let packet = frame
                    .write_ethernet_packet(&mut packet_buf, data)
                    .expect("Write Ethernet frame");

                tx.send_to(packet, None).unwrap().map_err(|e| {
                    log::error!("Failed to send packet: {e}");

                    ()
                })
            })
            .unwrap();

        Poll::Pending
    });

    // TODO: Unwraps
    let rx_task = smol::unblock(move || {
        loop {
            match rx.next() {
                Ok(ethernet_frame) => {
                    client_rx
                        .pdu_loop
                        .pdu_rx(ethernet_frame)
                        .map_err(|e| {
                            dbg!(ethernet_frame.len(), ethernet_frame);

                            e
                        })
                        .expect("RX");
                }
                Err(e) => {
                    // If an error occurs, we can handle it here
                    panic!("An error occurred while reading: {}", e);
                }
            }
        }
    });

    Ok(select::select(tx_task, rx_task))
}
