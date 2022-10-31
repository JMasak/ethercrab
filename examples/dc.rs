//! Configure Distributed Clocks (DC) for EK1100 and a couple of other modules.

use async_ctrlc::CtrlC;
use async_io::Timer;
use ethercrab::{error::Error, std::tx_rx_task, Client, PduLoop, SlaveGroup, Timeouts};
use futures_lite::{FutureExt, StreamExt};
use smol::LocalExecutor;
use std::{
    sync::{
        atomic::{AtomicU8, Ordering},
        Arc,
    },
    time::Duration,
};

#[cfg(target_os = "windows")]
// ASRock NIC
// const INTERFACE: &str = "TODO";
// // USB NIC
// const INTERFACE: &str = "\\Device\\NPF_{DCEDC919-0A20-47A2-9788-FC57D0169EDB}";
// Lenovo USB-C NIC
const INTERFACE: &str = "\\Device\\NPF_{CC0908D5-3CB8-46D6-B8A2-575D0578008D}";
// Silver USB NIC
// const INTERFACE: &str = "\\Device\\NPF_{CC0908D5-3CB8-46D6-B8A2-575D0578008D}";
#[cfg(not(target_os = "windows"))]
const INTERFACE: &str = "eth1";

const MAX_SLAVES: usize = 16;
const MAX_PDU_DATA: usize = 1100;
const MAX_FRAMES: usize = 16;
const PDI_LEN: usize = 64;

static PDU_LOOP: PduLoop<MAX_FRAMES, MAX_PDU_DATA, smol::Timer> = PduLoop::new();

async fn main_inner(ex: &LocalExecutor<'static>) -> Result<(), Error> {
    log::info!("Starting DC demo...");

    let client = Arc::new(Client::<MAX_FRAMES, MAX_PDU_DATA, smol::Timer>::new(
        &PDU_LOOP,
        Timeouts {
            wait_loop_delay: Duration::from_millis(0),
            ..Default::default()
        },
    ));

    ex.spawn(tx_rx_task(INTERFACE, &client).unwrap()).detach();

    let group = SlaveGroup::<MAX_SLAVES, PDI_LEN, MAX_FRAMES, MAX_PDU_DATA, _>::new(|_slave| {
        Box::pin(async { Ok(()) })
    });

    let group = client
        .init::<16, _>(group, |groups, slave| groups.push(slave))
        .await
        .expect("Init");

    log::info!("Group has {} slaves", group.slaves().len());

    for (slave, slave_stuff) in group.slaves().iter().enumerate() {
        let (_i, o) = group.io(slave).unwrap();

        log::info!(
            "-> Slave {slave} {} has outputs: {}",
            slave_stuff.name,
            o.is_some()
        );
    }

    // NOTE: This is currently hardcoded as 2ms inside the DC sync config, so keep them the same.
    let mut tick_interval = Timer::interval(Duration::from_millis(2));

    let group = Arc::new(group);
    let group2 = group.clone();

    // smol::spawn(async move {
    //     let mut cyclic_interval = Timer::interval(Duration::from_millis(2));

    //     while let Some(_) = cyclic_interval.next().await {
    //         group.tx_rx(&client).await.expect("TX/RX");
    //     }
    // })
    // .detach();

    while let Some(_) = tick_interval.next().await {
        group.tx_rx(&client).await.expect("TX/RX");

        // log::info!(
        //     "I {:?} O {:?}",
        //     group2.DELETEME_pdi_i(),
        //     group2.DELETEME_pdi_o()
        // );

        // let (_i, o) = group2.io(4).unwrap();

        // o.map(|o| {
        //     for byte in o.iter_mut() {
        //         *byte += 1;
        //     }
        // });

        for slave in 0..group2.slaves().len() {
            let (_i, o) = group2.io(slave).unwrap();

            o.map(|o| {
                for byte in o.iter_mut() {
                    *byte = !*byte;
                }
            });
        }
    }

    Ok(())
}

fn main() -> Result<(), Error> {
    env_logger::init();
    let local_ex = LocalExecutor::new();

    let ctrlc = CtrlC::new().expect("cannot create Ctrl+C handler?");

    futures_lite::future::block_on(
        local_ex.run(ctrlc.race(async { main_inner(&local_ex).await.unwrap() })),
    );

    Ok(())
}
