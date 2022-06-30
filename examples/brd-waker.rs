//! Similar to waker-list, it keeps a list of wakers, but this time will send a `BRD` service and
//! listen for its response by parsing the frame.

use async_ctrlc::CtrlC;
use core::cell::RefCell;
use core::sync::atomic::{AtomicU8, Ordering};
use core::task::Poll;
use core::task::Waker;
use ethercrab::pdu2::Pdu;
use ethercrab::register::RegisterAddress;
use futures_lite::FutureExt;
use mac_address::get_mac_address;
use pnet::datalink::{self, DataLinkReceiver, DataLinkSender};
use smol::LocalExecutor;
use smoltcp::wire::{EthernetAddress, EthernetFrame, EthernetProtocol, PrettyPrinter};
use std::sync::Arc;

#[cfg(target_os = "windows")]
// ASRock NIC
// const INTERFACE: &str = "TODO";
// USB NIC
const INTERFACE: &str = "\\Device\\NPF_{DCEDC919-0A20-47A2-9788-FC57D0169EDB}";
#[cfg(not(target_os = "windows"))]
const INTERFACE: &str = "eth0";

fn get_tx_rx() -> (Box<dyn DataLinkSender>, Box<dyn DataLinkReceiver>) {
    let interfaces = datalink::interfaces();

    dbg!(&interfaces);

    let interface = interfaces
        .into_iter()
        .find(|interface| interface.name == INTERFACE)
        .unwrap();

    dbg!(interface.mac);

    let (tx, rx) = match datalink::channel(&interface, Default::default()) {
        Ok(datalink::Channel::Ethernet(tx, rx)) => (tx, rx),
        Ok(_) => panic!("Unhandled channel type"),
        Err(e) => panic!(
            "An error occurred when creating the datalink channel: {}",
            e
        ),
    };

    (tx, rx)
}

fn main() {
    let local_ex = LocalExecutor::new();

    let (mut tx, mut rx) = get_tx_rx();

    // TODO: Hard code to some value. EtherCAT doesn't care about MAC if broadcast is always used
    let my_mac_addr = EthernetAddress::from_bytes(
        &get_mac_address()
            .expect("Failed to read MAC")
            .expect("No mac found")
            .bytes(),
    );

    let ctrlc = CtrlC::new().expect("cannot create Ctrl+C handler?");

    futures_lite::future::block_on(local_ex.run(ctrlc.race(async {
        let client = Arc::new(Client::<16, 16>::new());

        let client2 = client.clone();
        // let mut client3 = client.clone();

        // local_ex
        //     .spawn(async move {
        //     //
        //     })
        //     .detach();

        // MSRV: Use scoped threads in 1.63
        smol::spawn(smol::unblock(move || {
            loop {
                match rx.next() {
                    Ok(packet) => {
                        let packet = EthernetFrame::new_unchecked(packet);

                        if packet.ethertype() == EthernetProtocol::Unknown(0x88a4) {
                            // Ignore broadcast packets sent from self
                            if packet.src_addr() == my_mac_addr {
                                continue;
                            }

                            println!(
                                "Received EtherCAT packet. Source MAC {}, dest MAC {}",
                                packet.src_addr(),
                                packet.dst_addr()
                            );

                            client2.parse_response_ethernet_frame(packet.payload());
                        }
                    }
                    Err(e) => {
                        // If an error occurs, we can handle it here
                        panic!("An error occurred while reading: {}", e);
                    }
                }
            }
        }))
        .detach();

        let res = client
            .brd::<[u8; 1]>(RegisterAddress::Type, &mut tx)
            .await
            .unwrap();
        println!("RESULT: {:?}", res);
        let res = client
            .brd::<[u8; 2]>(RegisterAddress::Build, &mut tx)
            .await
            .unwrap();
        println!("RESULT: {:?}", res);
    })));
}

// #[derive(Clone)]
struct Client<const N: usize, const D: usize> {
    wakers: RefCell<[Option<Waker>; N]>,
    frames: RefCell<[Option<Pdu<D>>; N]>,
    idx: AtomicU8,
}

// TODO: Make sure this is ok
unsafe impl<const N: usize, const D: usize> Sync for Client<D, N> {}

impl<const N: usize, const D: usize> Client<N, D> {
    fn new() -> Self {
        assert!(
            N < u8::MAX.into(),
            "Packet indexes are u8s, so cache array cannot be any bigger than u8::MAX"
        );

        Self {
            wakers: RefCell::new([(); N].map(|_| None)),
            frames: RefCell::new([(); N].map(|_| None)),
            idx: AtomicU8::new(0),
        }
    }

    // TODO: Register address enum ETG1000.4 Table 31
    // TODO: Make `tx` a trait somehow so we can use it in both no_std and std
    pub async fn brd<T>(
        &self,
        address: RegisterAddress,
        tx: &mut Box<dyn DataLinkSender>,
    ) -> Result<T, ()>
    where
        T: PduData,
        for<'a> T: TryFrom<&'a [u8]>,
        for<'a> <T as TryFrom<&'a [u8]>>::Error: core::fmt::Debug,
    {
        let address = address as u16;
        let idx = self.idx.fetch_add(1, Ordering::Release) % N as u8;

        // We're receiving too fast or the receive buffer isn't long enough
        if self.frames.borrow()[usize::from(idx)].is_some() {
            println!("Index {idx} is already in use");

            return Err(());
        }

        println!("BRD {idx}");

        let data_length = T::len();

        let pdu = Pdu::<D>::brd(address, data_length, idx);

        let ethernet_frame = pdu_to_ethernet(&pdu);

        tx.send_to(&ethernet_frame.as_ref(), None)
            .unwrap()
            .expect("Send");

        let res = futures_lite::future::poll_fn(|ctx| {
            let removed = self.frames.borrow_mut()[usize::from(idx)].take();

            println!("poll_fn idx {} has data {:?}", idx, removed);

            if let Some(frame) = removed {
                println!("poll_fn -> Ready, data {:?}", frame.data);
                Poll::Ready(frame)
            } else {
                self.wakers.borrow_mut()[usize::from(idx)].replace(ctx.waker().clone());

                println!("poll_fn -> Pending, waker #{idx}");

                Poll::Pending
            }
        })
        .await;

        println!("Raw data {:?}", res.data.as_slice());

        res.data.as_slice().try_into().map_err(|e| {
            println!("{:?}", e);
            ()
        })
    }

    // TODO: Return a result if index is out of bounds, or we don't have a waiting packet
    pub fn parse_response_ethernet_frame(&self, ethernet_frame_payload: &[u8]) {
        let (rest, pdu) =
            Pdu::<D>::from_ethercat_frame_unchecked(&ethernet_frame_payload).expect("Packet parse");

        // TODO: Handle multiple PDUs here
        if !rest.is_empty() {
            println!("{} remaining bytes! (maybe just padding)", rest.len());
        }

        let idx = pdu.index;

        let waker = self.wakers.borrow_mut()[usize::from(idx)].take();

        println!("Looking for waker #{idx}: {:?}", waker);

        // Frame is ready; tell everyone about it
        if let Some(waker) = waker {
            // TODO: Validate PDU against the one stored with the waker.

            println!("Waker #{idx} found. Insert PDU {:?}", pdu);

            self.frames.borrow_mut()[usize::from(idx)].replace(pdu);
            waker.wake()
        }
    }
}

// TODO: Move into crate
trait PduData {
    const LEN: u16;

    fn len() -> u16 {
        Self::LEN & ethercrab::LEN_MASK
    }
}

impl PduData for u8 {
    const LEN: u16 = Self::BITS as u16 / 8;
}
impl PduData for u16 {
    const LEN: u16 = Self::BITS as u16 / 8;
}
impl<const N: usize> PduData for [u8; N] {
    const LEN: u16 = N as u16;
}

// TODO: Move into crate, pass buffer in instead of returning a vec
fn pdu_to_ethernet<const N: usize>(pdu: &Pdu<N>) -> EthernetFrame<Vec<u8>> {
    let src = get_mac_address()
        .expect("Failed to read MAC")
        .expect("No mac found");

    // Broadcast
    let dest = EthernetAddress::BROADCAST;

    let ethernet_len = EthernetFrame::<&[u8]>::buffer_len(pdu.frame_buf_len());

    let mut buffer = Vec::new();
    buffer.resize(ethernet_len, 0x00u8);

    let mut frame = EthernetFrame::new_checked(buffer).unwrap();

    pdu.as_ethercat_frame(&mut frame.payload_mut()).unwrap();
    frame.set_src_addr(EthernetAddress::from_bytes(&src.bytes()));
    frame.set_dst_addr(dest);
    // TODO: Const
    frame.set_ethertype(EthernetProtocol::Unknown(0x88a4));

    println!(
        "Send {}",
        PrettyPrinter::<EthernetFrame<&'static [u8]>>::new("", &frame)
    );

    frame
}