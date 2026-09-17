#![no_std]
#![no_main]
#[macro_use]
extern crate uom;

use defmt::info;
use embassy_executor::Spawner;
use embassy_stm32::wdg::IndependentWatchdog;
use embassy_stm32::{peripherals, Peri};
use embassy_time::{Timer};
use {defmt_rtt as _, panic_probe as _};

pub mod segments;
pub mod hardfault;
pub mod can;
pub mod clocks;
pub mod units;
pub mod broadcast;
pub mod debug;

use assign_resources::assign_resources;
assign_resources! {
    /// Resources for default task.
    default: DefaultResources {
        watchdog: IWDG,
    }
    /// Resources for CAN.
    can: CanResources {
        can: FDCAN2,
        can_tx: PB13,
        can_rx: PD9,
    }
    /// Resources for segment IsoSPI Line A.
    segment_isospi_linea: SegmentIsoSpiLineAResources {
        // Line A SPI config
        linea_spi: SPI1,
        linea_sck: PA5,
        linea_mosi: PD7,
        linea_miso: PA6,
        linea_cs: PG10,
        linea_tx_dma: GPDMA1_CH0,
        linea_rx_dma: GPDMA1_CH1,
    }
    /// Resources for segment IsoSPI Line B.
    segment_isospi_lineb: SegmentIsoSpiLineBResources {
        // Line B SPI config
        lineb_spi: SPI2,
        lineb_sck: PD3,
        lineb_mosi: PG1,
        lineb_miso: PC2,
        lineb_cs: PA3,
        lineb_tx_dma: GPDMA1_CH2,
        lineb_rx_dma: GPDMA1_CH3,
    }
}

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    info!("Initializing project...");

    let p = embassy_stm32::init(clocks::rcc_config());

    hardfault::report_last_reset();

    let r = split_resources!(p);

    spawner.spawn(can::can_task(spawner, r.can).expect("Failed to spawn can::can_task()."));
    spawner.spawn(default_task(r.default).expect("Failed to spawn default_task()."));
    spawner.spawn(segments::segments_task(r.segment_isospi_linea, r.segment_isospi_lineb).expect("Failed to spawn segments::segments_task()."));
    spawner.spawn(debug::segments_debug().expect("Failed to spawn debug::segments_debug()."));
}

/// pet the dog beat the heart
#[embassy_executor::task]
pub async fn default_task(r: DefaultResources) -> ! {
    /// Period between heartbeats. should be a good amount under `WATCHDOG_TIMEOUT_US`.
    const HEARTBEAT_PERIOD_MS: u64 = 500;
    /// Watchdog timeout in micros. if we stop petting for this long, the chip resets.
    const WATCHDOG_TIMEOUT_US: u32 = 1_000_000;

    let mut watchdog = IndependentWatchdog::new(r.watchdog, WATCHDOG_TIMEOUT_US);
    watchdog.unleash();

    let mut heartbeat_counter: usize = 0;

    loop {
        info!("heartbeat={}", &heartbeat_counter);
        heartbeat_counter += 1;
        Timer::after_millis(HEARTBEAT_PERIOD_MS).await;
        watchdog.pet();
    }
}
