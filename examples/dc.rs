//! Configure Distributed Clocks (DC) for EK1100 and a couple of other modules.

use async_ctrlc::CtrlC;
use async_io::Timer;
use ethercrab::{
    error::Error, std::tx_rx_task, Client, PduLoop, RegisterAddress, SlaveGroup, SubIndex, Timeouts,
};
use futures_lite::{FutureExt, StreamExt};
use smol::LocalExecutor;
use std::{
    sync::{
        atomic::{AtomicU8, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

#[cfg(target_os = "windows")]
// ASRock NIC
// const INTERFACE: &str = "\\Device\\NPF_{BEE3ADE3-E30E-4A64-BC67-C31320804CA9}";
// // USB NIC
// const INTERFACE: &str = "\\Device\\NPF_{DCEDC919-0A20-47A2-9788-FC57D0169EDB}";
// Lenovo USB-C NIC
const INTERFACE: &str = "\\Device\\NPF_{CC0908D5-3CB8-46D6-B8A2-575D0578008D}";
// Silver USB NIC
// const INTERFACE: &str = "\\Device\\NPF_{CC0908D5-3CB8-46D6-B8A2-575D0578008D}";
#[cfg(not(target_os = "windows"))]
const INTERFACE: &str = "eth0";

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
            wait_loop_delay: Duration::from_millis(2),
            mailbox_response: Duration::from_millis(1000),
            ..Default::default()
        },
    ));

    ex.spawn(tx_rx_task(INTERFACE, &client).unwrap()).detach();

    let group = SlaveGroup::<MAX_SLAVES, PDI_LEN, MAX_FRAMES, MAX_PDU_DATA, _>::new(|slave| {
        Box::pin(async {
            if slave.name() == "EL3004" {
                log::info!("Found EL3004. Configuring...");

                // Taken from TwinCAT
                slave.write_sdo(0x1c12, SubIndex::Index(0), 0u8).await?;
                slave.write_sdo(0x1c13, SubIndex::Index(0), 0u8).await?;

                slave
                    .write_sdo(0x1c13, SubIndex::Index(1), 0x1a00u16)
                    .await?;
                slave
                    .write_sdo(0x1c13, SubIndex::Index(2), 0x1a02u16)
                    .await?;
                slave
                    .write_sdo(0x1c13, SubIndex::Index(3), 0x1a04u16)
                    .await?;
                slave
                    .write_sdo(0x1c13, SubIndex::Index(4), 0x1a06u16)
                    .await?;
                slave.write_sdo(0x1c13, SubIndex::Index(0), 4u8).await?;
            } else if slave.name() == "LAN9252-EVB-HBI" {
                log::info!("Found LAN9252 in {:?} state", slave.state().await.ok());

                let sync_type = slave.read_sdo::<u16>(0x1c32, SubIndex::Index(1)).await?;
                let cycle_time = slave.read_sdo::<u32>(0x1c32, SubIndex::Index(2)).await?;
                let shift_time = slave
                    .read_sdo::<u32>(0x1c32, SubIndex::Index(3))
                    .await
                    .unwrap_or(0);
                log::info!("Outputs sync stuff {sync_type} {cycle_time} ns, shift {shift_time} ns");

                let sync_type = slave.read_sdo::<u16>(0x1c33, SubIndex::Index(1)).await?;
                let cycle_time = slave.read_sdo::<u32>(0x1c33, SubIndex::Index(2)).await?;
                let shift_time = slave
                    .read_sdo::<u32>(0x1c33, SubIndex::Index(3))
                    .await
                    .unwrap_or(0);
                log::info!("Inputs sync stuff {sync_type} {cycle_time} ns, shift {shift_time} ns");

                // Force IO into free run mode
                // slave.write_sdo(0x1c32, SubIndex::Index(1), 0u16).await?;
                // slave.write_sdo(0x1c33, SubIndex::Index(1), 0u16).await?;
            }

            Ok(())
        })
    });

    let group = client
        .init::<16, _>(group, |groups, slave| groups.push(slave))
        .await
        .expect("Init");

    log::info!("Group has {} slaves", group.slaves().len());

    for (slave, slave_stuff) in group.slaves().iter().enumerate() {
        let (i, o) = group.io(slave).unwrap();

        log::info!(
            "-> Slave {slave} {} has inputs: {}, outputs: {}",
            slave_stuff.name,
            i.is_some(),
            o.is_some()
        );
    }

    // NOTE: This is currently hardcoded as 2ms inside the DC sync config, so keep them the same.
    let mut tick_interval = Timer::interval(Duration::from_millis(5));

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

        // Dynamic drift compensation
        let (_reference_time, _wkc) = client
            .frmw::<u64>(0x1000, RegisterAddress::DcSystemTime)
            .await?;

        for slave in 0..group2.slaves().len() {
            let (_i, o) = group2.io(slave).unwrap();

            let diff = group2
                .slave(slave, &client)
                .unwrap()
                .raw_read::<u32>(RegisterAddress::DcSystemTimeDifference)
                .await
                .map(|n| {
                    let smaller = n & (1 << 31) > 0;

                    // Chop off smaller/larger bit
                    let number = (n & (u32::MAX >> 1)) as i32;

                    if smaller {
                        -number
                    } else {
                        number
                    }
                })
                .unwrap_or(0);

            print!("{diff:+#05} ");

            o.map(|o| {
                for byte in o.iter_mut() {
                    // *byte = !*byte;
                    *byte = byte.wrapping_add(1);
                }
            });
        }

        println!("");
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