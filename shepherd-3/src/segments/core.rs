//! Module holding the `Segments` struct. This is the main place where the Segments stuff is configured/set up. The main segments task (i.e., the one that owns the SPI peripheral and actually does the reads/cache updates) is also in here.

use static_cell::StaticCell;
use embassy_time::{Timer};
use super::chips::ADBMS6830B_NUM_CHIPS;

embassy_stm32::bind_interrupts!(struct Irqs {
    GPDMA1_CHANNEL0 => embassy_stm32::dma::InterruptHandler<embassy_stm32::peripherals::GPDMA1_CH0>;
    GPDMA1_CHANNEL1 => embassy_stm32::dma::InterruptHandler<embassy_stm32::peripherals::GPDMA1_CH1>;
    GPDMA1_CHANNEL2 => embassy_stm32::dma::InterruptHandler<embassy_stm32::peripherals::GPDMA1_CH2>;
    GPDMA1_CHANNEL3 => embassy_stm32::dma::InterruptHandler<embassy_stm32::peripherals::GPDMA1_CH3>;
});

pub mod alias {
    use embedded_hal_bus::spi::ExclusiveDevice;
    use embassy_time::Delay;
    use embassy_stm32::{
        mode::Async,
        gpio::Output,
        spi::{Spi, mode::Master},
    };
    use super::ADBMS6830B_NUM_CHIPS;

    /// Type alias representing a SPI controller that implements `SpiDevice` from `embedded_hal_async`.
    /// This is just a single SPI controller with a CS pin.
    pub type SpiDevice = ExclusiveDevice<Spi<'static, Async, Master>, Output<'static>, Delay>;

    /// The error type our `SpiDevice` produces.
    #[allow(unused)]
    pub type SpiError = <SpiDevice as embedded_hal_async::spi::ErrorType>::Error;

    /// Type alias representing an IsoSPI Line.
    ///
    /// Each line can go up to `ADBMS6830B_NUM_CHIPS` chips, but the actual number of chips they have is dynamic at runtime and is managed by the `Service`.
    pub type Line = adbms6830b::line::Line<SpiDevice, ADBMS6830B_NUM_CHIPS>;

    /// Type alias representing our adbms6830b Service configuration.
    pub type Service = adbms6830b::turnkey::service::Service<SpiDevice, ADBMS6830B_NUM_CHIPS>;

    /// Type alias for the `Api` (basically the same as `Service` but for the underlying API)
    pub type Api = adbms6830b::turnkey::api::Api<SpiDevice, ADBMS6830B_NUM_CHIPS>;
}

/// Guy in charge of the segments.
pub(super) struct Segments {
    pub(super) service: &'static mut alias::Service,
}
impl Segments {
    /// Initializes our `Segments`. AKA inits the two isoSPI lines. Doesn't start up the task or set any runtime config registers though.
    ///
    /// ### Parameters
    /// - `r_linea`: pins and other hardware resources for Line A
    /// - `r_lineb`: pins and other hardware resources for Line B
    pub fn new(r_linea: crate::SegmentIsoSpiLineAResources, r_lineb: crate::SegmentIsoSpiLineBResources) -> Self {
        use embedded_hal_bus::spi::ExclusiveDevice;
        use embassy_time::{Delay};
        use adbms6830b::turnkey::service::service_config::{ServiceConfig, SEGMENT_ISOSPI_EVAL_PERIOD_MS, SEGMENT_ISOSPI_MAX_FAILED_VERIFICATION_ATTEMPTS, SEGMENT_ISOSPI_MAX_SPLIT_ATTEMPTS, SEGMENT_ISOSPI_MIN_ATTEMPTS_FOR_FAIL, SEGMENT_ISOSPI_MIN_ATTEMPTS_TO_OPEN_WINDOW, SEGMENT_ISOSPI_PEC_FAILURE_RATIO_PCT, SEGMENT_ISOSPI_RECOVERY_STARTUP_TIME_MS};

        let mut spi_config = embassy_stm32::spi::Config::default();
        spi_config.frequency = embassy_stm32::time::mhz(1);

        let linea_spi = embassy_stm32::spi::Spi::new(r_linea.linea_spi, r_linea.linea_sck, r_linea.linea_mosi, r_linea.linea_miso, r_linea.linea_tx_dma, r_linea.linea_rx_dma, Irqs, spi_config);
        let linea_cs = embassy_stm32::gpio::Output::new(r_linea.linea_cs, embassy_stm32::gpio::Level::High, embassy_stm32::gpio::Speed::High);
        let linea_spi = ExclusiveDevice::new(linea_spi, linea_cs, Delay).unwrap();
        let line_a: alias::Line = adbms6830b::line::Line::new(linea_spi);

        let lineb_spi = embassy_stm32::spi::Spi::new(r_lineb.lineb_spi, r_lineb.lineb_sck, r_lineb.lineb_mosi, r_lineb.lineb_miso, r_lineb.lineb_tx_dma, r_lineb.lineb_rx_dma, Irqs, spi_config);
        let lineb_cs = embassy_stm32::gpio::Output::new(r_lineb.lineb_cs, embassy_stm32::gpio::Level::High, embassy_stm32::gpio::Speed::High);
        let lineb_spi: alias::SpiDevice = ExclusiveDevice::new(lineb_spi, lineb_cs, Delay).unwrap();
        let line_b: alias::Line = adbms6830b::line::Line::new(lineb_spi);

        static SERVICE: StaticCell<alias::Service> = StaticCell::new();
        let service: &'static mut alias::Service = SERVICE.init(alias::Service::new(
            line_a,
            line_b,
            ServiceConfig {
                // ik could just use `..Default::default()` but this is easier if we wanna change config in the future
                segment_isospi_eval_period_ms: SEGMENT_ISOSPI_EVAL_PERIOD_MS,
                segment_isospi_min_attempts_for_fail: SEGMENT_ISOSPI_MIN_ATTEMPTS_FOR_FAIL,
                segment_isospi_pec_failure_ratio_pct: SEGMENT_ISOSPI_PEC_FAILURE_RATIO_PCT,
                segment_isospi_min_attempts_to_open_window: SEGMENT_ISOSPI_MIN_ATTEMPTS_TO_OPEN_WINDOW,
                segment_isospi_max_split_attempts: SEGMENT_ISOSPI_MAX_SPLIT_ATTEMPTS,
                segment_isospi_max_failed_verification_attempts: SEGMENT_ISOSPI_MAX_FAILED_VERIFICATION_ATTEMPTS,
                segment_isospi_recovery_startup_time_ms: SEGMENT_ISOSPI_RECOVERY_STARTUP_TIME_MS,
            },
        ));

        Self { service }
    }

    /// Wrapper function to run the service and handle diagnostics.
    pub async fn run_service(&mut self) {
        // u_TODO: it would be cleaner if the on_startup async closure could be stored directly on the `Service` struct and defined via Service::new() rather than passed into service.run, since technically you shouldn't be able to change the startup routine each time you pass it into .run(). So possibly a good idea to look into that
        let diagnostics = self
            .service
            .run(
                // ADBMS6830B Service startup sequence! this gets called by the service at boot time, and whenever the service needs to restart the chips (isospi recovery or sleep detection)
                async |api, _reason| {
                    use adbms6830b::chip::registers::{
                        config_a::{
                            ConfigA,
                            types::{ReferenceOn, ComparisonThresholdVoltage, SoakTimeOn, SoakTimeRange, OpenWireSoakTimeMultiplier, GpioPullDownConfig, IirFilterConfig},
                        },
                        config_b::{
                            ConfigB,
                            types::{OvervoltageThreshold, UndervoltageThreshold, DischargeTimerMonitor, DischargeTimerStatus, DischargeTimerRange, DischargeCellConfig},
                        },
                    };
                    use adbms6830b::chip::{
                        commands,
                        commands::adc::{AdcvRedundancy, Acquisition, ResetFilter, OpenWire},
                    };
                    use adbms6830b::turnkey::service::StartupResult;

                    // Reset chips to blank state.
                    if let Err(err) = api.reset().await {
                        defmt::error!("Segments: Failed to call `api.reset()` during ADBMS6830B Service startup. Error: {}", err.to_kind());
                        return StartupResult::Incomplete;
                    }

                    // Set up ConfigA.
                    let config_a = const {
                        ConfigA::new()
                    .with_refon(ReferenceOn::On)
                    .with_cth(ComparisonThresholdVoltage::Mv25_05)
                    // not going to do `clear_diagnostic_flags()` like the C code since they are all cleared via ConfigA::new() and if were to manually re-clear them here it would have to be 8 separate calls for each flag
                    .with_soakon(SoakTimeOn::On)
                    .with_owrng(SoakTimeRange::Short)
                    .with_owa(OpenWireSoakTimeMultiplier::X1)
                    .with_fc(IirFilterConfig::Hz10)

                    // ConfigA: GPIO Pull-down settings
                    .with_gpio1(GpioPullDownConfig::PullDownOff)
                    .with_gpio2(GpioPullDownConfig::PullDownOff)
                    .with_gpio3(GpioPullDownConfig::PullDownOff)
                    .with_gpio4(GpioPullDownConfig::PullDownOff)
                    .with_gpio5(GpioPullDownConfig::PullDownOff)
                    .with_gpio6(GpioPullDownConfig::PullDownOff)
                    .with_gpio7(GpioPullDownConfig::PullDownOff)  // this is an on board therm for beta only
                    .with_gpio8(GpioPullDownConfig::PullDownOff)  // this is a on board therm
                    // set outputs, 9=iso led 10=bal LED. false=lit up
                    .with_gpio9(GpioPullDownConfig::PullDownOff)
                    .with_gpio10(GpioPullDownConfig::PullDownOff)
                    };
                    if let Err(err) = api.set_configa(&[config_a; super::chips::ADBMS6830B_NUM_CHIPS]).await {
                        defmt::error!("Segments: Failed to write ConfigA during ADBMS6830B Service startup. Error: {}", err);
                        return StartupResult::Incomplete;
                    }

                    // Set up ConfigB.
                    let config_b = const {
                        /// VOV setting from microvolts.
                        const VOV: OvervoltageThreshold = const {
                            const MICROVOLTS: i32 = 4_200_000; // 4.2 volts
                            OvervoltageThreshold::from_microvolts(MICROVOLTS).expect("Invalid OvervoltageThreshold for VOV.")
                        };

                        /// VUV setting from microvolts.
                        const VUV: UndervoltageThreshold = const {
                            const MICROVOLTS: i32 = 2_500_000; // 2.5 volts
                            UndervoltageThreshold::from_microvolts(MICROVOLTS).expect("Invalid OvervoltageThreshold for VUV.")
                        };

                        ConfigB::new()
                            .with_vov(VOV)
                            .with_vuv(VUV)
                            .with_dtmen(DischargeTimerMonitor::Disabled)
                            .with_dcto(DischargeTimerStatus::new().with_increments(0))
                            .with_dtrng(DischargeTimerRange::ShortRange)
                            .with_dcc1(DischargeCellConfig::ShortingSwitchOff)
                            .with_dcc2(DischargeCellConfig::ShortingSwitchOff)
                            .with_dcc3(DischargeCellConfig::ShortingSwitchOff)
                            .with_dcc4(DischargeCellConfig::ShortingSwitchOff)
                            .with_dcc5(DischargeCellConfig::ShortingSwitchOff)
                            .with_dcc6(DischargeCellConfig::ShortingSwitchOff)
                            .with_dcc7(DischargeCellConfig::ShortingSwitchOff)
                            .with_dcc8(DischargeCellConfig::ShortingSwitchOff)
                            .with_dcc9(DischargeCellConfig::ShortingSwitchOff)
                            .with_dcc10(DischargeCellConfig::ShortingSwitchOff)
                            .with_dcc11(DischargeCellConfig::ShortingSwitchOff)
                            .with_dcc12(DischargeCellConfig::ShortingSwitchOff)
                            .with_dcc13(DischargeCellConfig::ShortingSwitchOff)
                            .with_dcc14(DischargeCellConfig::ShortingSwitchOff)
                            .with_dcc15(DischargeCellConfig::ShortingSwitchOff)
                            .with_dcc16(DischargeCellConfig::ShortingSwitchOff)
                    };
                    if let Err(err) = api.set_configb(&[config_b; ADBMS6830B_NUM_CHIPS]).await {
                        defmt::error!("Segments: Failed to write ConfigB during ADBMS6830B Service startup. Error: {}", err);
                        return StartupResult::Incomplete;
                    }

                    // Disable balancing on init.
                    if let Err(err) = api.command(commands::discharge::mute()).await {
                        defmt::error!("Segments: Failed to send `mute()` command to disable balancing during ADBMS6830B Service startup. Error = {}", err.to_kind());
                        return StartupResult::Incomplete;
                    }

                    // Start the ADCV conversions.
                    if let Err(err) = api.command(commands::adc::adcv(AdcvRedundancy::Enabled, Acquisition::Continuous, ResetFilter::Reset, OpenWire::OffForAll)).await {
                        defmt::error!("Segments: Failed to send command to start adcv() conversions during ADBMS6830B Service startup. Error = {}", err.to_kind());
                        return StartupResult::Incomplete;
                    }

                    // okay startup is complete now
                    // we have to delay after init is successful for 500ms to wait for ADC to start up (this is what the C code does)
                    Timer::after_millis(500).await;
                    StartupResult::Complete
                },
            )
            .await;

        // handle diagnostics
        {
            // u_TODO: eventually this should probably go into its own (flaggable) task, since it doesn't do any SPI transactions itself. it never awaits as of rn so it's probably fine for now.

            let diagnostics_started = embassy_time::Instant::now();

            // accumulator diagnostics (not per-chip ones)
            let accumulator = diagnostics.accumulator();
            defmt_monitor::monitor!("Segments/ServiceDiagnostics/Accumulator/PreviousState", desc = "State of the accumulator accumulator prior to this Service cycle. This is the state failed, attempts, and failure_pct were gathered under.", "{}", accumulator.previous_state());
            defmt_monitor::monitor!("Segments/ServiceDiagnostics/Accumulator/State", desc = "Current state of the accumulator window. This is the state resulting from the failed, attempts, and failure_pct values.", "{}", accumulator.state());
            defmt_monitor::monitor!("Segments/ServiceDiagnostics/Accumulator/FailurePctThreshold", desc = "Percentage of reads that must fail their PEC for a chip to be considered as \"failing\". Note: This is a constant value!", "{=u8}", accumulator.failure_pct_threshold());
            defmt_monitor::monitor!("Segments/ServiceDiagnostics/Accumulator/AccumulatorWindowPeriod", desc = "How long the accumulator evaluation window lasts, in ms. Basically, after a window opens, this is how long the window stays open to gather PEC data. Note: This is a constant value!", "{=u64}", accumulator.accumulator_window_period());
            defmt_monitor::monitor!("Segments/ServiceDiagnostics/Accumulator/MinAttemptsForFail", desc = "Fewest reads a chip must have taken part in before its failure rate is actually considered as meaning anything. Note: This is a constant value!", "{=usize}", accumulator.min_attempts_for_fail());
            defmt_monitor::monitor!("Segments/ServiceDiagnostics/Accumulator/BelowMinAttemptsToOpenWindowCount", desc = "This counts the number of times a chip has passed over opening a window because it hadn't taken part in SEGMENT_ISOSPI_MIN_ATTEMPTS_TO_OPEN_WINDOW reads since the last time the Service has run.", "{=usize}", accumulator.below_min_attempts_to_open_window_count());
            defmt_monitor::monitor!("Segments/ServiceDiagnostics/Accumulator/BelowMinAttemptsForFailCount", desc = "This counts the number of times a chip could not be judged failed or not because it hadn't taken part in SEGMENT_ISOSPI_MIN_ATTEMPTS_FOR_FAIL reads over the whole window.", "{=usize}", accumulator.below_min_attempts_for_fail_count());
            defmt_monitor::monitor!("Segments/ServiceDiagnostics/Accumulator/MinAttemptsToOpenWindow", desc = "Fewest reads in a single update before that update's failure rate can open a window. Note: This is a constant value!", "{=usize}", accumulator.min_attempts_to_open_window());
            defmt_monitor::monitor!("Segments/ServiceDiagnostics/Accumulator/PecMask", desc = "Current PEC mask state.", "{}", accumulator.pec_mask());
            defmt_monitor::monitor!("Segments/ServiceDiagnostics/Accumulator/RecoveryStartupTime", desc = "Length of grace period that occurs after startup or after a sleep, where the accumulator doesn't accumulate PEC errors. Note: This is a constant value!", "{=u64}", accumulator.recovery_startup_time());
            defmt_monitor::monitor!("Segments/ServiceDiagnostics/Accumulator/UpdatesWhileMaskedCount", desc = "This counts the number of times update_chips() (and therefore update() itself) has run while a PEC mask is active.", "{=usize}", accumulator.updates_while_masked_count());

            // accumulator diagnostics (.failed() ones).
            let failed = accumulator.failed();
            defmt_monitor::monitor!("Segments/ServiceDiagnostics/Accumulator/PerChip/Chip0/failed", desc = "Total PEC failures counted for this chip during this window.", "{=usize}", failed[0]);
            defmt_monitor::monitor!("Segments/ServiceDiagnostics/Accumulator/PerChip/Chip1/failed", desc = "Total PEC failures counted for this chip during this window.", "{=usize}", failed[1]);
            defmt_monitor::monitor!("Segments/ServiceDiagnostics/Accumulator/PerChip/Chip2/failed", desc = "Total PEC failures counted for this chip during this window.", "{=usize}", failed[2]);
            defmt_monitor::monitor!("Segments/ServiceDiagnostics/Accumulator/PerChip/Chip3/failed", desc = "Total PEC failures counted for this chip during this window.", "{=usize}", failed[3]);
            defmt_monitor::monitor!("Segments/ServiceDiagnostics/Accumulator/PerChip/Chip4/failed", desc = "Total PEC failures counted for this chip during this window.", "{=usize}", failed[4]);
            defmt_monitor::monitor!("Segments/ServiceDiagnostics/Accumulator/PerChip/Chip5/failed", desc = "Total PEC failures counted for this chip during this window.", "{=usize}", failed[5]);
            defmt_monitor::monitor!("Segments/ServiceDiagnostics/Accumulator/PerChip/Chip6/failed", desc = "Total PEC failures counted for this chip during this window.", "{=usize}", failed[6]);
            defmt_monitor::monitor!("Segments/ServiceDiagnostics/Accumulator/PerChip/Chip7/failed", desc = "Total PEC failures counted for this chip during this window.", "{=usize}", failed[7]);
            defmt_monitor::monitor!("Segments/ServiceDiagnostics/Accumulator/PerChip/Chip8/failed", desc = "Total PEC failures counted for this chip during this window.", "{=usize}", failed[8]);
            defmt_monitor::monitor!("Segments/ServiceDiagnostics/Accumulator/PerChip/Chip9/failed", desc = "Total PEC failures counted for this chip during this window.", "{=usize}", failed[9]);

            // accumulator diagnostics (.attempts() ones).
            let attempts = accumulator.attempts();
            defmt_monitor::monitor!("Segments/ServiceDiagnostics/Accumulator/PerChip/Chip0/attempts", desc = "Total read attempts counted for this chip during this window.", "{=usize}", attempts[0]);
            defmt_monitor::monitor!("Segments/ServiceDiagnostics/Accumulator/PerChip/Chip1/attempts", desc = "Total read attempts counted for this chip during this window.", "{=usize}", attempts[1]);
            defmt_monitor::monitor!("Segments/ServiceDiagnostics/Accumulator/PerChip/Chip2/attempts", desc = "Total read attempts counted for this chip during this window.", "{=usize}", attempts[2]);
            defmt_monitor::monitor!("Segments/ServiceDiagnostics/Accumulator/PerChip/Chip3/attempts", desc = "Total read attempts counted for this chip during this window.", "{=usize}", attempts[3]);
            defmt_monitor::monitor!("Segments/ServiceDiagnostics/Accumulator/PerChip/Chip4/attempts", desc = "Total read attempts counted for this chip during this window.", "{=usize}", attempts[4]);
            defmt_monitor::monitor!("Segments/ServiceDiagnostics/Accumulator/PerChip/Chip5/attempts", desc = "Total read attempts counted for this chip during this window.", "{=usize}", attempts[5]);
            defmt_monitor::monitor!("Segments/ServiceDiagnostics/Accumulator/PerChip/Chip6/attempts", desc = "Total read attempts counted for this chip during this window.", "{=usize}", attempts[6]);
            defmt_monitor::monitor!("Segments/ServiceDiagnostics/Accumulator/PerChip/Chip7/attempts", desc = "Total read attempts counted for this chip during this window.", "{=usize}", attempts[7]);
            defmt_monitor::monitor!("Segments/ServiceDiagnostics/Accumulator/PerChip/Chip8/attempts", desc = "Total read attempts counted for this chip during this window.", "{=usize}", attempts[8]);
            defmt_monitor::monitor!("Segments/ServiceDiagnostics/Accumulator/PerChip/Chip9/attempts", desc = "Total read attempts counted for this chip during this window.", "{=usize}", attempts[9]);

            // accumulator diagnostics (.failure_pct() ones).
            let failure_pct = accumulator.failure_pct();
            defmt_monitor::monitor!("Segments/ServiceDiagnostics/Accumulator/PerChip/Chip0/failure_pct", desc = "This chip's PEC failure rate over the current window as a percentage (0 - 100).", "{=u8}", failure_pct[0]);
            defmt_monitor::monitor!("Segments/ServiceDiagnostics/Accumulator/PerChip/Chip1/failure_pct", desc = "This chip's PEC failure rate over the current window as a percentage (0 - 100).", "{=u8}", failure_pct[1]);
            defmt_monitor::monitor!("Segments/ServiceDiagnostics/Accumulator/PerChip/Chip2/failure_pct", desc = "This chip's PEC failure rate over the current window as a percentage (0 - 100).", "{=u8}", failure_pct[2]);
            defmt_monitor::monitor!("Segments/ServiceDiagnostics/Accumulator/PerChip/Chip3/failure_pct", desc = "This chip's PEC failure rate over the current window as a percentage (0 - 100).", "{=u8}", failure_pct[3]);
            defmt_monitor::monitor!("Segments/ServiceDiagnostics/Accumulator/PerChip/Chip4/failure_pct", desc = "This chip's PEC failure rate over the current window as a percentage (0 - 100).", "{=u8}", failure_pct[4]);
            defmt_monitor::monitor!("Segments/ServiceDiagnostics/Accumulator/PerChip/Chip5/failure_pct", desc = "This chip's PEC failure rate over the current window as a percentage (0 - 100).", "{=u8}", failure_pct[5]);
            defmt_monitor::monitor!("Segments/ServiceDiagnostics/Accumulator/PerChip/Chip6/failure_pct", desc = "This chip's PEC failure rate over the current window as a percentage (0 - 100).", "{=u8}", failure_pct[6]);
            defmt_monitor::monitor!("Segments/ServiceDiagnostics/Accumulator/PerChip/Chip7/failure_pct", desc = "This chip's PEC failure rate over the current window as a percentage (0 - 100).", "{=u8}", failure_pct[7]);
            defmt_monitor::monitor!("Segments/ServiceDiagnostics/Accumulator/PerChip/Chip8/failure_pct", desc = "This chip's PEC failure rate over the current window as a percentage (0 - 100).", "{=u8}", failure_pct[8]);
            defmt_monitor::monitor!("Segments/ServiceDiagnostics/Accumulator/PerChip/Chip9/failure_pct", desc = "This chip's PEC failure rate over the current window as a percentage (0 - 100).", "{=u8}", failure_pct[9]);

            // timing diagnostics
            let timing = diagnostics.timing();
            defmt_monitor::monitor!(
                "Segments/ServiceDiagnostics/Timing/period",
                desc = "The difference in time between the most recent Service cycle, and the Service cycle before that. In ms. Will be 0ms if fewer than two Service cycles have run yet.",
                "{=u64}",
                match timing.period() {
                    Some(duration) => duration.as_millis(),
                    None => 0,
                }
            );
            defmt_monitor::monitor!("Segments/ServiceDiagnostics/Timing/max_period", desc = "The maximum period the Service has observed while running. In ms.", "{=u64}", timing.max_period().as_millis());
            defmt_monitor::monitor!("Segments/ServiceDiagnostics/Timing/work", desc = "How long the “work” of the Service took during the most recent Service cycle. In us.", "{=u64}", timing.work().as_micros());
            defmt_monitor::monitor!("Segments/ServiceDiagnostics/Timing/max_work", desc = "The maximum work the Service has observed while running. In us.", "{=u64}", timing.max_work().as_micros());

            // chip state diagnostics
            let chipstate = diagnostics.chip_state_diagnostics();
            /// macro for the chipstate diagnostics since i don't want to copy paste this. the only parameter is the index of the chip
            macro_rules! chipstate_diagnostics {
                ($val:literal) => {
                    let state: adbms6830b::turnkey::api::ChipState = chipstate.chip_state()[$val];
                    let line: adbms6830b::turnkey::api::LineId = chipstate.chip_line()[$val];
                    let command_count = state.command_count();
                    defmt_monitor::monitor!(["Segments/ServiceDiagnostics/ChipState/Chip", $val, "/line"], desc = "Which Line this chip is on.", "{}", line);
                    defmt_monitor::monitor!(["Segments/ServiceDiagnostics/ChipState/Chip", $val, "/pec_failed_count"], desc = "Total number of times this chip has read in a failed PEC. (different to what the accumulator reports, since this is overall)", "{=usize}", state.pec_failed_count());
                    defmt_monitor::monitor!(["Segments/ServiceDiagnostics/ChipState/Chip", $val, "/pec_success_count"], desc = "Total number of times this chip has read in a successful PEC. (different to what the accumulator reports, since this is overall)", "{=usize}", state.pec_success_count());
                    defmt_monitor::monitor!(["Segments/ServiceDiagnostics/ChipState/Chip", $val, "/command_count_resets"], desc = "Number of times the command counter for this chip has been reset due to a sleep.", "{=usize}", state.command_count_resets());
                    defmt_monitor::monitor!(
                        ["Segments/ServiceDiagnostics/ChipState/Chip", $val, "/last_contacted"],
                        desc = "Last time we heard from this chip with a good PEC. In ms since system boot. 0ms if we have never heard from this chip.",
                        "{=u64}",
                        match state.last_contacted() {
                            Some(instant) => instant.as_millis(),
                            None => 0,
                        }
                    );
                    defmt_monitor::monitor!(["Segments/ServiceDiagnostics/ChipState/Chip", $val, "/CommandCount/expected"], desc = "What this chip’s counter “should” be. This is tracked from the commands sent to it.", "{=u8}", command_count.expected());
                    defmt_monitor::monitor!(["Segments/ServiceDiagnostics/ChipState/Chip", $val, "/CommandCount/reported"], desc = "What this chip reported on the last read of it that passed its PEC.", "{=u8}", command_count.reported());
                    defmt_monitor::monitor!(["Segments/ServiceDiagnostics/ChipState/Chip", $val, "/CommandCount/in_sync"], desc = "Whether the reported counter matches the expected one. This can be expected to be false in some cases, like after isoSPI recovers from a break.", "{=bool}", command_count.in_sync());
                };
            }
            chipstate_diagnostics!(0);
            chipstate_diagnostics!(1);
            chipstate_diagnostics!(2);
            chipstate_diagnostics!(3);
            chipstate_diagnostics!(4);
            chipstate_diagnostics!(5);
            chipstate_diagnostics!(6);
            chipstate_diagnostics!(7);
            chipstate_diagnostics!(8);
            chipstate_diagnostics!(9);

            // general service diagnostics
            defmt_monitor::monitor!("Segments/ServiceDiagnostics/sleep_detection_spi_error_count", desc = "Number of times sleep detection has failed due to a SPI::Error.", "{=usize}", diagnostics.sleep_detection_spi_error_count());
            defmt_monitor::monitor!("Segments/ServiceDiagnostics/break_detection_spi_error_count", desc = "Number of times break detection has failed due to a SPI::Error.", "{=usize}", diagnostics.break_detection_spi_error_count());
            defmt_monitor::monitor!("Segments/ServiceDiagnostics/cycles_count", desc = "Number of times the service has ran so far. This increments on every loop the service makes.", "{=usize}", diagnostics.cycles_count());
            defmt_monitor::monitor!("Segments/ServiceDiagnostics/split", desc = "The current split of chips between the isoSPI lines. (you can also see the per-chip line reports if that's easier to read)", "{}", diagnostics.split());
            defmt_monitor::monitor!("Segments/ServiceDiagnostics/max_split_attempts", desc = "The configured maximum split attempts for isoSPI recovery. This is a const value!", "{=usize}", diagnostics.max_split_attempts());
            defmt_monitor::monitor!("Segments/ServiceDiagnostics/max_verification_attempts", desc = "The configured maximum verification attempts for isoSPI recovery. This is a const value!", "{=usize}", diagnostics.max_verification_attempts());

            // line diagnostics
            let line = diagnostics.line_diagnostics();
            defmt_monitor::monitor!("Segments/ServiceDiagnostics/Line/line_a_error_count", desc = "Total number of times Line A has failed with a SPI::Error.", "{=usize}", line.line_a_error_count());
            defmt_monitor::monitor!("Segments/ServiceDiagnostics/Line/most_recent_line_a_error", desc = "Most recent Error that has occured on Line A. None if no errors have occured yet. (this is for HAL-level errors, and has nothing to do with PEC errors or anything like that)", "{}", line.most_recent_line_a_error());
            defmt_monitor::monitor!("Segments/ServiceDiagnostics/Line/line_b_error_count", desc = "Total number of times Line B has failed with a SPI::Error.", "{=usize}", line.line_b_error_count());
            defmt_monitor::monitor!("Segments/ServiceDiagnostics/Line/most_recent_line_b_error", desc = "Most recent Error that has occured on Line B. None if no errors have occured yet. (this is for HAL-level errors, and has nothing to do with PEC errors or anything like that)", "{}", line.most_recent_line_b_error());
            defmt_monitor::monitor!("Segments/ServiceDiagnostics/Line/line_a_chips_detected_count", desc = "Current chips detected as REACHABLE on Line A. Not at all linked to the official line split. Can be affected by PEC noise. Will report an error if detection failed this service cycle.", "{}", line.line_a_chips_detected_count());
            defmt_monitor::monitor!("Segments/ServiceDiagnostics/Line/line_b_chips_detected_count", desc = "Current chips detected as REACHABLE on Line B. Not at all linked to the official line split. Can be affected by PEC noise. Will report an error if detection failed this service cycle.", "{}", line.line_b_chips_detected_count());

            // how long the logging in this closure took
            defmt_monitor::monitor!("Segments/ServiceDiagnostics/Timing/diagnostics_overhead", desc = "How long the ServiceDiagnostics logging took. In micros", "{=u64}", embassy_time::Instant::now().saturating_duration_since(diagnostics_started).as_micros());
        }
    }
}

/// "Jobs" to help the Segments task. Basically a collection of helper functions and blocks of work the segments task has to do.
pub mod jobs {
    use embassy_sync::blocking_mutex::{Mutex, raw::ThreadModeRawMutex};
    use embassy_time::{Duration, Instant};
    use crate::segments::cache::UpdateError;
    use crate::segments::cache;
    use super::Segments;

    pub struct JobDiagnosticsContainer {
        inner: Mutex<ThreadModeRawMutex, JobDiagnostics>,
    }
    impl JobDiagnosticsContainer {
        /// Initializes the JobDiagnostics to its defaults. Meant to be called only once at init time.
        const fn new() -> Self {
            Self { inner: Mutex::new(JobDiagnostics::new()) }
        }

        /// Updates the JobDiagnostics with new data after a successful job run has completed.
        /// This should be called right at the end of the job when you are about to return (i.e., where it is no longer possible for errors to occur and the job is known to have been successful).
        ///
        /// ### Parameters
        /// - `start_time`: The instant at which the job started. The caller should save this at the top of their job function. This function will then grab the current `Instant::now()` as `end_time` and calculate the duration the job took.
        fn update_with_successful_run(&self, start_time: Instant) {
            let end_time = Instant::now();
            let job_duration: Duration = end_time.saturating_duration_since(start_time);

            // SAFETY: this call doesn't nest calls to another lock or lock_mut closure
            unsafe {
                self.inner.lock_mut(|data| {
                    data.last_job_duration = job_duration;

                    if data.last_job_duration > data.max_job_duration {
                        data.max_job_duration = data.last_job_duration;
                    }

                    if data.last_job_duration < data.min_job_duration {
                        data.min_job_duration = data.last_job_duration;
                    }
                });
            }
        }

        /// Updates the JobDiagnostics after a failure has occured.
        /// This should be called whenever a job has to return early due to a failure. This doesn't record any duration metrics, and just increments the error count.
        /// This doesn't record the specific error or anything, so it is up to the job to do any more specific logging via defmt and such. JobDiagnostics mainly just keeps a log
        /// to ensure the history isn't lost.
        fn update_with_failure(&self) {
            // SAFETY: this call doesn't nest calls to another lock or lock_mut closure
            unsafe {
                self.inner.lock_mut(|data| {
                    data.error_count += 1;
                })
            }
        }

        /// Copies out the current inner `JobDiagnostics`.
        fn copy_inner(&self) -> JobDiagnostics {
            self.inner.lock(|inner| *inner)
        }
    }

    /// Diagnostic data for a segments job, mainly just used to track timing stuff (i.e., how long SPI reads and cache updates take).
    #[derive(Copy, Clone)]
    pub struct JobDiagnostics {
        /// How long the job took to run the last time it was ran.
        last_job_duration: Duration,
        /// The maximum time it took the job to run recorded so far.
        max_job_duration: Duration,
        /// The minimum time it took the job to run recorded so far.
        min_job_duration: Duration,
        /// Times this job was unable to run to completion due to an error.
        error_count: usize,
    }
    impl JobDiagnostics {
        /// Initializes the JobDiagnostics to its defaults. Meant to be called only once at init time.
        const fn new() -> Self {
            Self {
                last_job_duration: Duration::MIN,
                max_job_duration: Duration::MIN,
                min_job_duration: Duration::MAX,
                error_count: 0,
            }
        }

        /// How long the job took to run the last time it was ran.
        pub const fn last_job_duration(&self) -> Duration {
            self.last_job_duration
        }
        /// The maximum time it took the job to run recorded so far.
        pub const fn max_job_duration(&self) -> Duration {
            self.max_job_duration
        }
        /// The minimum time it took the job to run recorded so far.
        pub const fn min_job_duration(&self) -> Duration {
            self.min_job_duration
        }
        /// Times this job was unable to run to completion due to an error.
        pub const fn error_count(&self) -> usize {
            self.error_count
        }
    }

    /// Helper "jobs" that update the register caches and do stuff with SPI.
    impl Segments {
        /// Updates RedundantAux cache. Triggers a conversion, polls it, and then reads the result over SPI into the cache.
        pub async fn job_update_redundant_aux(&mut self) -> Result<JobDiagnostics, UpdateError> {
            use adbms6830b::chip::commands::adc::Aux2InputSelection;

            static DIAGNOSTICS: JobDiagnosticsContainer = JobDiagnosticsContainer::new();

            let start_time = Instant::now();

            /// Autoconvert timeout in ms.
            const TIMEOUT_MS: u64 = 100;

            // trigger the conversion and poll it until done
            if let Err(err) = self.service.api().adax2_autoconvert(Aux2InputSelection::All, TIMEOUT_MS).await {
                defmt::error!("Segments: Inside `job_update_redundant_aux()`: call to `adax2_autoconvert()` resulted in an error. Error: {}", err);
                DIAGNOSTICS.update_with_failure();
                return Err(UpdateError::PollError(err));
            }

            // actually read them
            if let Err(err) = cache::CACHE.update_redundant_aux(self.service.api()).await {
                defmt::error!("Segments: Inside `job_update_redundant_aux()`: cache update via call to `update_redundant_aux()` failed. Error: {}", err);
                DIAGNOSTICS.update_with_failure();
                return Err(err);
            }

            DIAGNOSTICS.update_with_successful_run(start_time);
            Ok(DIAGNOSTICS.copy_inner())
        }

        /// Updates the registers that require the SNAP command. This includes the following registers: CellVoltages, AverageCellVoltages, FilteredCellVoltages, SVotlages, StatusC, StatusD.
        ///
        /// This is done as a single job so there is only one SNAP window. This allows the data from the registers to be compared coherently.
        pub async fn job_update_snap_registers(&mut self) -> Result<JobDiagnostics, UpdateError> {
            use adbms6830b::chip::commands;

            static DIAGNOSTICS: JobDiagnosticsContainer = JobDiagnosticsContainer::new();

            let start_time = Instant::now();

            // when this job returns early, it doesn't unsnap the registers before doing so. this is fine because this job contains all the registers that are affected by SNAP. so other jobs
            // can still run as normal.

            // Snap the registers before doing anything!
            if let Err(err) = self.service.api().command(commands::snapshot::snap()).await {
                defmt::error!("Segments: Inside `job_update_snap_registers()`: Failed to send the SNAP command. Error: {}", err);
                DIAGNOSTICS.update_with_failure();
                return Err(UpdateError::SnapError(err));
            }

            // Update cell voltages.
            if let Err(err) = cache::CACHE.update_cell_voltages(self.service.api()).await {
                defmt::error!("Segments: Inside `job_update_snap_registers()`: Failed to call `update_cell_voltages()`. Error: {}", err);
                DIAGNOSTICS.update_with_failure();
                return Err(err);
            }

            // Update average cell voltages.
            if let Err(err) = cache::CACHE.update_average_cell_voltages(self.service.api()).await {
                defmt::error!("Segments: Inside `job_update_snap_registers()`: Failed to call `update_average_cell_voltages()`. Error: {}", err);
                DIAGNOSTICS.update_with_failure();
                return Err(err);
            }

            // Update filtered cell voltages.
            if let Err(err) = cache::CACHE.update_filtered_cell_voltages(self.service.api()).await {
                defmt::error!("Segments: Inside `job_update_snap_registers()`: Failed to call `update_filtered_cell_voltages()`. Error: {}", err);
                DIAGNOSTICS.update_with_failure();
                return Err(err);
            }

            // Update S voltages.
            if let Err(err) = cache::CACHE.update_s_voltages(self.service.api()).await {
                defmt::error!("Segments: Inside `job_update_snap_registers()`: Failed to call `update_s_voltages()`. Error: {}", err);
                DIAGNOSTICS.update_with_failure();
                return Err(err);
            }

            // Update StatusC.
            if let Err(err) = cache::CACHE.update_status_c(self.service.api()).await {
                defmt::error!("Segments: Inside `job_update_snap_registers()`: Failed to call `update_status_c()`. Error: {}", err);
                DIAGNOSTICS.update_with_failure();
                return Err(err);
            }

            // Update StatusD.
            if let Err(err) = cache::CACHE.update_status_d(self.service.api()).await {
                defmt::error!("Segments: Inside `job_update_snap_registers()`: Failed to call `update_status_d()`. Error: {}", err);
                DIAGNOSTICS.update_with_failure();
                return Err(err);
            }

            // Unsnap.
            if let Err(err) = self.service.api().command(commands::snapshot::unsnap()).await {
                defmt::error!("Segments: Inside `job_update_snap_registers()`: Failed to send the UNSNAP command. Error: {}", err);
                DIAGNOSTICS.update_with_failure();
                return Err(UpdateError::UnsnapError(err));
            }

            DIAGNOSTICS.update_with_successful_run(start_time);
            Ok(DIAGNOSTICS.copy_inner())
        }

        /// Update the Auxillary registers cache. Triggers a conversion, polls it, and then reads the result over SPI into the cache.
        pub async fn job_update_aux_registers(&mut self) -> Result<JobDiagnostics, UpdateError> {
            use adbms6830b::chip::commands::adc::{Aux1InputSelection, OpenWireAux, Pull};

            static DIAGNOSTICS: JobDiagnosticsContainer = JobDiagnosticsContainer::new();

            let start_time = Instant::now();

            /// Autoconvert timeout in ms.
            const TIMEOUT_MS: u64 = 100;

            // need to run autoconvert to update the data we read
            if let Err(err) = self.service.api().adax_autoconvert(OpenWireAux::Off, Pull::PullDown, Aux1InputSelection::All, TIMEOUT_MS).await {
                defmt::error!("Segments: Inside `job_update_aux_registers()`: call to `adax_autoconvert()` resulted in an error. Error: {}", err);
                DIAGNOSTICS.update_with_failure();
                return Err(UpdateError::PollError(err));
            }

            // Update AuxillaryA through D.
            if let Err(err) = cache::CACHE.update_aux(self.service.api()).await {
                defmt::error!("Segments: Inside `job_update_aux_registers()`: Failed to call `update_aux()`. Error: {}", err);
                DIAGNOSTICS.update_with_failure();
                return Err(err);
            }

            // Update StatusA.
            if let Err(err) = cache::CACHE.update_status_a(self.service.api()).await {
                defmt::error!("Segments: Inside `job_update_aux_registers()`: Failed to call `update_status_a()`. Error: {}", err);
                DIAGNOSTICS.update_with_failure();
                return Err(err);
            }

            // Update StatusB.
            if let Err(err) = cache::CACHE.update_status_b(self.service.api()).await {
                defmt::error!("Segments: Inside `job_update_aux_registers()`: Failed to call `update_status_b()`. Error: {}", err);
                DIAGNOSTICS.update_with_failure();
                return Err(err);
            }

            DIAGNOSTICS.update_with_successful_run(start_time);
            Ok(DIAGNOSTICS.copy_inner())
        }

        /// Update the PWM registers cache.
        pub async fn job_update_pwm_registers(&mut self) -> Result<JobDiagnostics, UpdateError> {
            static DIAGNOSTICS: JobDiagnosticsContainer = JobDiagnosticsContainer::new();

            let start_time = Instant::now();

            // Update PwmA and PwmB.
            if let Err(err) = cache::CACHE.update_pwm(self.service.api()).await {
                defmt::error!("Segments: Inside `job_update_pwm_registers()`: Failed to call `update_pwm()`. Error: {}", err);
                DIAGNOSTICS.update_with_failure();
                return Err(err);
            }

            DIAGNOSTICS.update_with_successful_run(start_time);
            Ok(DIAGNOSTICS.copy_inner())
        }
    }
}

/// Module for the main Segments task. (this is the task that owns the segments SPI peripheral and does all the actual SPI reads and cache updates. any other tasks just read the cached data but don't actually make any spi commands themselves)
pub mod task {
    use super::{Segments};
    use crate::broadcast::Broadcast;
    use embassy_sync::blocking_mutex::{raw::ThreadModeRawMutex};
    use embassy_time::{Instant, Duration, Timer};

    /// `Broadcast` static for segments task. This allows the Segments task to flag other tasks when it successfully runs the jobs.
    ///
    /// This allows other tasks to .await until fresh cache data is available for them to read.
    pub mod signal {
        use super::*;

        const SEGMENTS_FRESH_DATA_MAX_WAITERS: usize = 10;
        pub static SEGMENTS_FRESH_DATA_SIGNAL: Broadcast<ThreadModeRawMutex, SEGMENTS_FRESH_DATA_MAX_WAITERS> = Broadcast::new();
    }

    /// Main task in charge of managing the segments.
    ///
    /// This task owns the SPI peripheral. It handles all SPI transactions with the ADBMS6830B chips and all cache updates.
    ///
    /// In general this task should only really do stuff that requires ownership of the SPI peripheral. It should just make reads for cache updates, and maybe some conditional reads based on BMS state. It
    /// shouldn't do any processing on that read data though. Once the data is cached, it should generally be read by other tasks since reading the data doesn't require making any actual SPI transactions.
    #[embassy_executor::task]
    pub async fn segments_task(r_linea: crate::SegmentIsoSpiLineAResources, r_lineb: crate::SegmentIsoSpiLineBResources) {
        /// Frequency (in ms) at which the segments task should run.
        const SEGMENTS_TASK_FREQUENCY_MS: u64 = 300;

        let mut segments = Segments::new(r_linea, r_lineb);

        /// Timing diagnostics for the segments task.
        struct Diagnostics {
            last_duration: Duration,
            min_duration: Duration,
            max_duration: Duration,
        }
        impl Diagnostics {
            pub const fn new() -> Self {
                Diagnostics {
                    last_duration: Duration::MIN,
                    min_duration: Duration::MAX,
                    max_duration: Duration::MIN,
                }
            }

            fn update(&mut self, start_time: Instant) {
                let end_time = Instant::now();
                let duration: Duration = end_time.saturating_duration_since(start_time);

                self.last_duration = duration;
                if self.last_duration > self.max_duration {
                    self.max_duration = self.last_duration;
                }
                if self.last_duration < self.min_duration {
                    self.min_duration = self.last_duration;
                }
            }

            fn log(&self) {
                defmt_monitor::monitor!("Segments/TaskDiagnostics/last_duration", desc = "Time (in milliseconds) it took for the segments task to run the last time it ran.", "{=u64}", self.last_duration.as_millis());
                defmt_monitor::monitor!("Segments/TaskDiagnostics/max_duration", desc = "Maximum time (in milliseconds) we have observed the segments task taking to run so far.", "{=u64}", self.max_duration.as_millis());
                defmt_monitor::monitor!("Segments/TaskDiagnostics/min_duration", desc = "Minimum time (in milliseconds) we have observed the segments task taking to run so far.", "{=u64}", self.min_duration.as_millis());
            }
        }

        let mut diagnostics = Diagnostics::new();

        loop {
            let start_time = Instant::now();

            // Run the segments service. (this handles isoSPI detection/recovery and sleep detection).
            segments.run_service().await;

            // Do the SPI transactions to update the register caches.
            '_cacheupdates: {
                let mut all_successful: bool = true;

                // Run update aux registers job.
                match segments.job_update_aux_registers().await {
                    Ok(diagnostics) => {
                        defmt_monitor::monitor!("Segments/JobDiagnostics/job_update_aux_registers()/last_job_duration", desc = "Time (in milliseconds) it took for this job to run the last time it ran.", "{=u64}", diagnostics.last_job_duration().as_millis());
                        defmt_monitor::monitor!("Segments/JobDiagnostics/job_update_aux_registers()/max_job_duration", desc = "Maximum time (in milliseconds) we have observed this job taking to run so far.", "{=u64}", diagnostics.max_job_duration().as_millis());
                        defmt_monitor::monitor!("Segments/JobDiagnostics/job_update_aux_registers()/min_job_duration", desc = "Minimum time (in milliseconds) we have observed this job taking to run so far.", "{=u64}", diagnostics.min_job_duration().as_millis());
                        defmt_monitor::monitor!("Segments/JobDiagnostics/job_update_aux_registers()/error_count", desc = "Number of times this job has had to return early due to an error.", "{=usize}", diagnostics.error_count());
                    },
                    Err(err) => {
                        defmt::error!("Segments: Inside `segments_task()`: Failed to call `job_update_aux_registers()`. Error: {}", err);
                        all_successful = false;
                    },
                }

                // Run update snap registers job.
                match segments.job_update_snap_registers().await {
                    Ok(diagnostics) => {
                        defmt_monitor::monitor!("Segments/JobDiagnostics/job_update_snap_registers()/last_job_duration", desc = "Time (in milliseconds) it took for this job to run the last time it ran.", "{=u64}", diagnostics.last_job_duration().as_millis());
                        defmt_monitor::monitor!("Segments/JobDiagnostics/job_update_snap_registers()/max_job_duration", desc = "Maximum time (in milliseconds) we have observed this job taking to run so far.", "{=u64}", diagnostics.max_job_duration().as_millis());
                        defmt_monitor::monitor!("Segments/JobDiagnostics/job_update_snap_registers()/min_job_duration", desc = "Minimum time (in milliseconds) we have observed this job taking to run so far.", "{=u64}", diagnostics.min_job_duration().as_millis());
                        defmt_monitor::monitor!("Segments/JobDiagnostics/job_update_snap_registers()/error_count", desc = "Number of times this job has had to return early due to an error.", "{=usize}", diagnostics.error_count());
                    },
                    Err(err) => {
                        defmt::error!("Segments: Inside `segments_task()`: Failed to call `job_update_snap_registers()`. Error: {}", err);
                        all_successful = false;
                    },
                }

                // Run update redundant aux registers job.
                match segments.job_update_redundant_aux().await {
                    Ok(diagnostics) => {
                        defmt_monitor::monitor!("Segments/JobDiagnostics/job_update_redundant_aux()/last_job_duration", desc = "Time (in milliseconds) it took for this job to run the last time it ran.", "{=u64}", diagnostics.last_job_duration().as_millis());
                        defmt_monitor::monitor!("Segments/JobDiagnostics/job_update_redundant_aux()/max_job_duration", desc = "Maximum time (in milliseconds) we have observed this job taking to run so far.", "{=u64}", diagnostics.max_job_duration().as_millis());
                        defmt_monitor::monitor!("Segments/JobDiagnostics/job_update_redundant_aux()/min_job_duration", desc = "Minimum time (in milliseconds) we have observed this job taking to run so far.", "{=u64}", diagnostics.min_job_duration().as_millis());
                        defmt_monitor::monitor!("Segments/JobDiagnostics/job_update_redundant_aux()/error_count", desc = "Number of times this job has had to return early due to an error.", "{=usize}", diagnostics.error_count());
                    },
                    Err(err) => {
                        defmt::error!("Segments: Inside `segments_task()`: Failed to call `job_update_redundant_aux()`. Error: {}", err);
                        all_successful = false;
                    },
                }

                // Run update PWM registers job.
                match segments.job_update_pwm_registers().await {
                    Ok(diagnostics) => {
                        defmt_monitor::monitor!("Segments/JobDiagnostics/job_update_pwm_registers()/last_job_duration", desc = "Time (in milliseconds) it took for this job to run the last time it ran.", "{=u64}", diagnostics.last_job_duration().as_millis());
                        defmt_monitor::monitor!("Segments/JobDiagnostics/job_update_pwm_registers()/max_job_duration", desc = "Maximum time (in milliseconds) we have observed this job taking to run so far.", "{=u64}", diagnostics.max_job_duration().as_millis());
                        defmt_monitor::monitor!("Segments/JobDiagnostics/job_update_pwm_registers()/min_job_duration", desc = "Minimum time (in milliseconds) we have observed this job taking to run so far.", "{=u64}", diagnostics.min_job_duration().as_millis());
                        defmt_monitor::monitor!("Segments/JobDiagnostics/job_update_pwm_registers()/error_count", desc = "Number of times this job has had to return early due to an error.", "{=usize}", diagnostics.error_count());
                    },
                    Err(err) => {
                        defmt::error!("Segments: Inside `segments_task()`: Failed to call `job_update_pwm_registers()`. Error: {}", err);
                        all_successful = false;
                    },
                }

                if all_successful {
                    signal::SEGMENTS_FRESH_DATA_SIGNAL.signal();
                }
            }

            diagnostics.update(start_time);
            diagnostics.log();

            // u_TODO maybe eventually try making this a Ticker or something, for now though this is closest to tx_thread_sleep so probably should keep it for now until we know everything else works
            Timer::after_millis(SEGMENTS_TASK_FREQUENCY_MS).await;
        }
    }
}
