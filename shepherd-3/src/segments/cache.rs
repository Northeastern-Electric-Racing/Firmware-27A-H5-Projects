//! Module for caching SPI reads to the ADBMS6830B chips.
//!
//! (this module uses `Cell` because the cache can be accessed at any time by different tasks. it doesn't use RefCell because that can panic. maybe in the future it would be good to look into RefCell but the Cell copies are realistically never going to be an actual issue)

use adbms6830b::{
    chip::registers::{
        ReadableGroup,
        pwm::{PwmA, PwmB},
        results::{RedundantAuxillaryA, RedundantAuxillaryB, RedundantAuxillaryC, RedundantAuxillaryD},
        results::{CellVoltagesA, CellVoltagesB, CellVoltagesC, CellVoltagesD, CellVoltagesE},
        results::{AverageCellVoltagesA, AverageCellVoltagesB, AverageCellVoltagesC, AverageCellVoltagesD, AverageCellVoltagesE},
        results::{FilteredCellVoltagesA, FilteredCellVoltagesB, FilteredCellVoltagesC, FilteredCellVoltagesD, FilteredCellVoltagesE},
        results::{SVoltagesA, SVoltagesB, SVoltagesC, SVoltagesD, SVoltagesE},
        status::{StatusC, StatusD, StatusA, StatusB},
        clear::{ClearFlags, types::ClearAction, ClearOvervoltageUndervoltage},
        results::{AuxillaryA, AuxillaryB, AuxillaryC, AuxillaryD},
    },
    turnkey::api::LineId,
};
use adbms6830b::line::Error;
use crate::segments::core::alias::{SpiError};
use adbms6830b::line::PecStatus;
use core::cell::{Cell};
use super::chips::IndexByChip;
use super::core::alias;
use super::chips::ADBMS6830B_NUM_CHIPS;

/// Cache to hold read data.
pub(super) static CACHE: CacheData = CacheData::new();

/// Errors that may occur when trying to update a value in the cache.
#[derive(Clone, Copy, Debug)]
#[derive(defmt::Format)]
pub enum UpdateError {
    /// Error occurred while trying to run the UNSNAP command.
    UnsnapError(Error<SpiError>),
    /// Error occurred while trying to run the SNAP command.
    SnapError(Error<SpiError>),
    /// Error occurred while trying to clear flags after reading them in an update call.
    ClearFlagsError(Error<SpiError>),
    /// Error occurred while polling a conversion completion (possibly via a ...autoconvert() function).
    PollError(Error<SpiError>),
    /// Line A failed during update. Inner contains the SPI error.
    LineAFailed(Error<SpiError>),
    /// Line B failed during update. Inner contains the SPI error.
    LineBFailed(Error<SpiError>),
    /// Both lines failed during update. Inner contains both SPI errors.
    BothLinesFailed { linea_err: Error<SpiError>, lineb_err: Error<SpiError> },
    /// Impossible error that should not be possible to happen. Using this instead of unreachable!()
    /// or an unwrap/expect so an impossible error that somehow happens doesn't panic the whole bms
    ImpossibleError,
}

/// Register reading for a single chip.
#[derive(Copy, Clone, Debug)]
pub struct Reading<R: ReadableGroup> {
    data: R,
    pec: PecStatus,
}
impl<R: ReadableGroup> Reading<R> {
    /// Actual register reading.
    pub const fn data(&self) -> R {
        self.data
    }
    /// The PEC status of the reading.
    pub const fn pec(&self) -> PecStatus {
        self.pec
    }
}

/// Actual register cache data (held inside blocking mutex)
#[derive(Copy, Clone)]
pub struct RegisterCacheData<R: ReadableGroup> {
    /// Contains the read data for each chip. Starts out as `None` if this register hasn't been cached yet.
    data: Option<IndexByChip<Reading<R>>>,
    /// Last instant this register cache was successfully read over SPI and updated.
    /// If no read has been made yet, this is None.
    last_sucessful_read: Option<embassy_time::Instant>,
}
// ^^ u_TODO ideas for maybe cool extra stuff we could add to RegisterCacheData:
// - a `read_duration: Option<embassy_time::Duration>` that stores how long the most recent read took. maybe also a `max_read_duration`, `min_read_duration`, and `avg_read_duration` field? would actually be helpful for optimizing our timing a bit. or just cool to look at
// - idk

impl<R: ReadableGroup> RegisterCacheData<R> {
    /// Last instant this register cache was successfully read over SPI and updated.
    /// If no read has been made yet, this is None.
    pub const fn last_sucessful_read(&self) -> Option<embassy_time::Instant> {
        self.last_sucessful_read
    }

    /// Register read data for each chip.
    /// If no read has been made yet, this is None.
    pub const fn data(&self) -> &Option<IndexByChip<Reading<R>>> {
        &self.data
    }
}

pub struct RegisterCache<R: ReadableGroup> {
    inner: embassy_sync::blocking_mutex::ThreadModeMutex<Cell<RegisterCacheData<R>>>,
}

impl<R: ReadableGroup> RegisterCache<R> {
    /// New uninitialized register cache.
    pub const fn new() -> Self {
        Self {
            inner: embassy_sync::blocking_mutex::ThreadModeMutex::new(Cell::new(RegisterCacheData { data: None, last_sucessful_read: None })),
        }
    }

    /// Copies out Register Cache data. Copy is needed here due to the mutex, since multiple threads read the cache. Hopefully compiler uses RVO?
    pub fn data(&self) -> RegisterCacheData<R> {
        self.inner.lock(|inner| inner.get())
    }

    /// Reads the register and updates the cache.
    pub async fn update(&self, api: &mut alias::Api) -> Result<(), UpdateError> {
        let data: [Reading<R>; ADBMS6830B_NUM_CHIPS] = {
            let responses = api.read::<R>().await;

            match (responses.line_error(LineId::A), responses.line_error(LineId::B)) {
                // Both lines failed.
                (Some(linea_err), Some(lineb_err)) => {
                    defmt::error!("Segments: cache: In RegisterCache::update(): SPI Read on both Line A and Line B failed. Errors: linea_err={}, lineb_err={}", linea_err, lineb_err);
                    return Err(UpdateError::BothLinesFailed { linea_err: *linea_err, lineb_err: *lineb_err });
                },

                // Line A failed, but not Line B.
                (Some(linea_err), None) => {
                    defmt::error!("Segments: cache: In RegisterCache::update(): SPI Read on Line A failed. Error: {}", linea_err);
                    return Err(UpdateError::LineAFailed(*linea_err));
                },

                // Line B failed, but not Line A.
                (None, Some(lineb_err)) => {
                    defmt::error!("Segments: cache: In RegisterCache::update(): SPI Read on Line B failed. Error: {}", lineb_err);
                    return Err(UpdateError::LineBFailed(*lineb_err));
                },

                // Neither line failed so we're good
                (None, None) => (),
            }

            let readings: [Reading<R>; ADBMS6830B_NUM_CHIPS] = {
                let Some(readings) = responses.iter().map(|response| response.map(|response| Reading { data: response.data(), pec: response.pec() })).collect::<Option<heapless::Vec<Reading<R>, { ADBMS6830B_NUM_CHIPS }>>>().and_then(|readings| readings.into_array::<{ ADBMS6830B_NUM_CHIPS }>().ok()) else {
                    // u_Note: there is probably a way to restructure this so that ImpossibleError doesn't need to exist at all, but it might require going into the driver which is kinda annoying. so even though this existing is kinda gross it is probably fine for now
                    defmt::error!("Segments: cache: In RegisterCache::update(): a chip reading was `None` even though we already verified that no line errors occured. This should not be possible.");
                    return Err(UpdateError::ImpossibleError);
                };

                readings
            };

            readings
        };

        self.inner.lock(|inner| {
            inner.set(RegisterCacheData {
                data: Some(IndexByChip::new(data)),
                last_sucessful_read: Some(embassy_time::Instant::now()),
            });
        });

        Ok(())
    }
}

pub mod fault_counts {
    use crate::segments::chips::cells::{IndexByCell, CellId};
    use super::{CacheData, IndexByChip};

    /// Undervoltage/overvoltage flags from StatusD.
    pub mod undervotlage_overvoltage {
        use super::*;

        /// Undervoltage/overvoltage flags from StatusD.
        #[derive(Copy, Clone, Debug, defmt::Format)]
        pub struct UndervoltageOvervoltageFlags {
            pub c1uv: u32,
            pub c1ov: u32,

            pub c2uv: u32,
            pub c2ov: u32,

            pub c3uv: u32,
            pub c3ov: u32,

            pub c4uv: u32,
            pub c4ov: u32,

            pub c5uv: u32,
            pub c5ov: u32,

            pub c6uv: u32,
            pub c6ov: u32,

            pub c7uv: u32,
            pub c7ov: u32,

            pub c8uv: u32,
            pub c8ov: u32,

            pub c9uv: u32,
            pub c9ov: u32,

            pub c10uv: u32,
            pub c10ov: u32,

            pub c11uv: u32,
            pub c11ov: u32,

            pub c12uv: u32,
            pub c12ov: u32,

            pub c13uv: u32,
            pub c13ov: u32,

            pub c14uv: u32,
            pub c14ov: u32,

            pub c15uv: u32,
            pub c15ov: u32,

            pub c16uv: u32,
            pub c16ov: u32,
        }

        /// Undervoltage/overvoltage flag counts for a single cell.
        pub struct CellFlagCounts {
            /// How many times this cell's undervoltage flag has been read in as set.
            pub uv: u32,
            /// How many times this cell's overvoltage flag has been read in as set.
            pub ov: u32,
        }

        impl UndervoltageOvervoltageFlags {
            /// Lets you index the overvoltage/undervoltage flags by cell. This throws away c14uv/ov through c16uv/ov since we only have 13 cells.
            pub fn idx_by_cell(&self) -> IndexByCell<CellFlagCounts> {
                IndexByCell::from_fn(|cell| match cell {
                    CellId::Cell1 => CellFlagCounts { uv: self.c1uv, ov: self.c1ov },
                    CellId::Cell2 => CellFlagCounts { uv: self.c2uv, ov: self.c2ov },
                    CellId::Cell3 => CellFlagCounts { uv: self.c3uv, ov: self.c3ov },
                    CellId::Cell4 => CellFlagCounts { uv: self.c4uv, ov: self.c4ov },
                    CellId::Cell5 => CellFlagCounts { uv: self.c5uv, ov: self.c5ov },
                    CellId::Cell6 => CellFlagCounts { uv: self.c6uv, ov: self.c6ov },
                    CellId::Cell7 => CellFlagCounts { uv: self.c7uv, ov: self.c7ov },
                    CellId::Cell8 => CellFlagCounts { uv: self.c8uv, ov: self.c8ov },
                    CellId::Cell9 => CellFlagCounts { uv: self.c9uv, ov: self.c9ov },
                    CellId::Cell10 => CellFlagCounts { uv: self.c10uv, ov: self.c10ov },
                    CellId::Cell11 => CellFlagCounts { uv: self.c11uv, ov: self.c11ov },
                    CellId::Cell12 => CellFlagCounts { uv: self.c12uv, ov: self.c12ov },
                    CellId::Cell13 => CellFlagCounts { uv: self.c13uv, ov: self.c13ov },
                })
            }
        }
    }

    /// Comparison fault flags from StatusC.
    pub mod comparison_faults {
        use super::*;

        /// Comparison fault flags from StatusC.
        #[derive(Copy, Clone, Debug, defmt::Format)]
        pub struct ComparisonFaultFlags {
            pub cs1flt: u32,
            pub cs2flt: u32,
            pub cs3flt: u32,
            pub cs4flt: u32,
            pub cs5flt: u32,
            pub cs6flt: u32,
            pub cs7flt: u32,
            pub cs8flt: u32,
            pub cs9flt: u32,
            pub cs10flt: u32,
            pub cs11flt: u32,
            pub cs12flt: u32,
            pub cs13flt: u32,
            pub cs14flt: u32,
            pub cs15flt: u32,
            pub cs16flt: u32,
        }
        impl ComparisonFaultFlags {
            /// Lets you index the comparison fault flags by cell. This throws away cs14flt through cs16flt since we only have 13 cells.
            pub fn idx_by_cell(&self) -> IndexByCell<u32> {
                IndexByCell::from_fn(|cell| match cell {
                    CellId::Cell1 => self.cs1flt,
                    CellId::Cell2 => self.cs2flt,
                    CellId::Cell3 => self.cs3flt,
                    CellId::Cell4 => self.cs4flt,
                    CellId::Cell5 => self.cs5flt,
                    CellId::Cell6 => self.cs6flt,
                    CellId::Cell7 => self.cs7flt,
                    CellId::Cell8 => self.cs8flt,
                    CellId::Cell9 => self.cs9flt,
                    CellId::Cell10 => self.cs10flt,
                    CellId::Cell11 => self.cs11flt,
                    CellId::Cell12 => self.cs12flt,
                    CellId::Cell13 => self.cs13flt,
                })
            }
        }
    }

    /// Persistent counts of ADBMS6830B fault flags for CacheData.
    ///
    /// Basically, any time a fault flag gets read in here during an update (e.g., the fault flags on StatusC), that fault flag will be
    /// incremented here. The fault flags are W1C and are cleared each update, so this may help callers track the history of faults that
    /// have shown up, or detect that a fault occured during a cache read that they may have missed.
    ///
    /// Also, this isn't meant to be a super sophisticated error-detection thing. It is supposed to be pretty dumb and just a basic relay of fault values. If
    /// these are increased, it might not necessarily mean that something super serious is going on, it's just possibly useful data for debugging and diagnostic stuff.
    #[derive(Copy, Clone, Debug, defmt::Format)]
    pub struct FaultCounts {
        /// Source: StatusC.
        pub csxflt: comparison_faults::ComparisonFaultFlags,
        /// Source: StatusC.
        pub smed: u32,
        /// Source: StatusC.
        pub sed: u32,
        /// Source: StatusC.
        pub cmed: u32,
        /// Source: StatusC.
        pub ced: u32,
        /// Source: StatusC.
        pub vd_uv: u32,
        /// Source: StatusC.
        pub vd_ov: u32,
        /// Source: StatusC.
        pub va_uv: u32,
        /// Source: StatusC.
        pub va_ov: u32,
        /// Source: StatusC.
        pub oscchk: u32,
        /// Source: StatusC.
        pub tmodchk: u32,
        /// Source: StatusC.
        pub thsd: u32,
        /// Source: StatusC.
        pub sleep: u32,
        /// Source: StatusC.
        pub spiflt: u32,
        /// Source: StatusC.
        pub vde: u32,
        /// Source: StatusC.
        pub vdel: u32,

        /// Source: StatusD.
        pub cxovuv: undervotlage_overvoltage::UndervoltageOvervoltageFlags,
    }
    impl FaultCounts {
        /// Default FaultCounts where everything is zeroed.
        pub const fn new() -> Self {
            Self {
                csxflt: comparison_faults::ComparisonFaultFlags {
                    cs1flt: 0,
                    cs2flt: 0,
                    cs3flt: 0,
                    cs4flt: 0,
                    cs5flt: 0,
                    cs6flt: 0,
                    cs7flt: 0,
                    cs8flt: 0,
                    cs9flt: 0,
                    cs10flt: 0,
                    cs11flt: 0,
                    cs12flt: 0,
                    cs13flt: 0,
                    cs14flt: 0,
                    cs15flt: 0,
                    cs16flt: 0,
                },
                smed: 0,
                sed: 0,
                cmed: 0,
                ced: 0,
                vd_uv: 0,
                vd_ov: 0,
                va_uv: 0,
                va_ov: 0,
                oscchk: 0,
                tmodchk: 0,
                thsd: 0,
                sleep: 0,
                spiflt: 0,
                vde: 0,
                vdel: 0,
                cxovuv: undervotlage_overvoltage::UndervoltageOvervoltageFlags {
                    c1uv: 0,
                    c1ov: 0,
                    c2uv: 0,
                    c2ov: 0,
                    c3uv: 0,
                    c3ov: 0,
                    c4uv: 0,
                    c4ov: 0,
                    c5uv: 0,
                    c5ov: 0,
                    c6uv: 0,
                    c6ov: 0,
                    c7uv: 0,
                    c7ov: 0,
                    c8uv: 0,
                    c8ov: 0,
                    c9uv: 0,
                    c9ov: 0,
                    c10uv: 0,
                    c10ov: 0,
                    c11uv: 0,
                    c11ov: 0,
                    c12uv: 0,
                    c12ov: 0,
                    c13uv: 0,
                    c13ov: 0,
                    c14uv: 0,
                    c14ov: 0,
                    c15uv: 0,
                    c15ov: 0,
                    c16uv: 0,
                    c16ov: 0,
                },
            }
        }
    }

    impl CacheData {
        /// Gets persistent fault counts.
        pub fn get_fault_counts(&self) -> IndexByChip<FaultCounts> {
            self.fault_counts.lock(|inner| inner.get())
        }
    }
}

pub struct CacheData {
    fault_counts: embassy_sync::blocking_mutex::ThreadModeMutex<Cell<IndexByChip<fault_counts::FaultCounts>>>,

    raxa: RegisterCache<RedundantAuxillaryA>,
    raxb: RegisterCache<RedundantAuxillaryB>,
    raxc: RegisterCache<RedundantAuxillaryC>,
    raxd: RegisterCache<RedundantAuxillaryD>,

    cva: RegisterCache<CellVoltagesA>,
    cvb: RegisterCache<CellVoltagesB>,
    cvc: RegisterCache<CellVoltagesC>,
    cvd: RegisterCache<CellVoltagesD>,
    cve: RegisterCache<CellVoltagesE>,

    aca: RegisterCache<AverageCellVoltagesA>,
    acb: RegisterCache<AverageCellVoltagesB>,
    acc: RegisterCache<AverageCellVoltagesC>,
    acd: RegisterCache<AverageCellVoltagesD>,
    ace: RegisterCache<AverageCellVoltagesE>,

    fca: RegisterCache<FilteredCellVoltagesA>,
    fcb: RegisterCache<FilteredCellVoltagesB>,
    fcc: RegisterCache<FilteredCellVoltagesC>,
    fcd: RegisterCache<FilteredCellVoltagesD>,
    fce: RegisterCache<FilteredCellVoltagesE>,

    sca: RegisterCache<SVoltagesA>,
    scb: RegisterCache<SVoltagesB>,
    scc: RegisterCache<SVoltagesC>,
    scd: RegisterCache<SVoltagesD>,
    sce: RegisterCache<SVoltagesE>,

    statc: RegisterCache<StatusC>,

    statd: RegisterCache<StatusD>,

    auxa: RegisterCache<AuxillaryA>,
    auxb: RegisterCache<AuxillaryB>,
    auxc: RegisterCache<AuxillaryC>,
    auxd: RegisterCache<AuxillaryD>,

    stata: RegisterCache<StatusA>,

    statb: RegisterCache<StatusB>,

    pwma: RegisterCache<PwmA>,
    pwmb: RegisterCache<PwmB>,
}
impl CacheData {
    pub(super) const fn new() -> Self {
        Self {
            fault_counts: embassy_sync::blocking_mutex::ThreadModeMutex::new(Cell::new(IndexByChip::new([fault_counts::FaultCounts::new(); ADBMS6830B_NUM_CHIPS]))),

            raxa: RegisterCache::new(),
            raxb: RegisterCache::new(),
            raxc: RegisterCache::new(),
            raxd: RegisterCache::new(),

            cva: RegisterCache::new(),
            cvb: RegisterCache::new(),
            cvc: RegisterCache::new(),
            cvd: RegisterCache::new(),
            cve: RegisterCache::new(),

            aca: RegisterCache::new(),
            acb: RegisterCache::new(),
            acc: RegisterCache::new(),
            acd: RegisterCache::new(),
            ace: RegisterCache::new(),

            fca: RegisterCache::new(),
            fcb: RegisterCache::new(),
            fcc: RegisterCache::new(),
            fcd: RegisterCache::new(),
            fce: RegisterCache::new(),

            sca: RegisterCache::new(),
            scb: RegisterCache::new(),
            scc: RegisterCache::new(),
            scd: RegisterCache::new(),
            sce: RegisterCache::new(),

            statc: RegisterCache::new(),

            statd: RegisterCache::new(),

            auxa: RegisterCache::new(),
            auxb: RegisterCache::new(),
            auxc: RegisterCache::new(),
            auxd: RegisterCache::new(),

            stata: RegisterCache::new(),

            statb: RegisterCache::new(),

            pwma: RegisterCache::new(),
            pwmb: RegisterCache::new(),
        }
    }
}

/// Register groups RedundantAuxillaryA through D.
pub mod redundant_aux {
    use super::*;
    use crate::units::Voltage;
    use uom::si::{electric_potential::microvolt};
    use super::alias;
    use crate::segments::chips::gpios::{GpioId, IndexByGpio};

    /// Raw Redundant Aux register readings.
    pub struct Raw {
        pub raxa: RegisterCacheData<RedundantAuxillaryA>,
        pub raxb: RegisterCacheData<RedundantAuxillaryB>,
        pub raxc: RegisterCacheData<RedundantAuxillaryC>,
        pub raxd: RegisterCacheData<RedundantAuxillaryD>,
    }
    impl Raw {
        /// Tries to make it nice.
        ///
        /// If no data has been read to that cache yet, this will return `Err(())`.
        pub fn try_nice(&self) -> Result<NiceData, ()> {
            NiceData::try_from(self)
        }
    }
    // ^^ note: this struct is just meant to be a nice helper for formatting returned data. the `CacheData` struct is still meant to directly hold these registers itself

    /// "Nice data" for a single chip.
    pub struct NiceDataChip {
        inner: IndexByGpio<Voltage>,
    }
    impl core::ops::Deref for NiceDataChip {
        type Target = IndexByGpio<Voltage>;

        fn deref(&self) -> &Self::Target {
            &self.inner
        }
    }

    /// Represents the raw register readings, but formatted in a more readable way.
    ///
    /// This doesn't contain any metadata about the reading (e.g., PEC errors). So you should
    /// probably inspect that stuff from the `Raw` readings before converting to this.
    pub struct NiceData {
        inner: IndexByChip<NiceDataChip>,
    }
    impl core::ops::Deref for NiceData {
        type Target = IndexByChip<NiceDataChip>;

        fn deref(&self) -> &Self::Target {
            &self.inner
        }
    }
    impl TryFrom<&Raw> for NiceData {
        type Error = ();

        /// Attempts to convert `Raw` data into a `NiceData`. If any of the registers involved
        /// in this `NiceData` haven't been read yet, this returns `Err(())`.
        fn try_from(raw: &Raw) -> Result<Self, Self::Error> {
            let Some(a) = raw.raxa.data() else {
                return Err(());
            };
            let Some(b) = raw.raxb.data() else {
                return Err(());
            };
            let Some(c) = raw.raxc.data() else {
                return Err(());
            };
            let Some(d) = raw.raxd.data() else {
                return Err(());
            };

            Ok(Self {
                inner: {
                    IndexByChip::from_fn(|chip| NiceDataChip {
                        inner: IndexByGpio::from_fn(|gpio| match gpio {
                            GpioId::Gpio1 => Voltage::new::<microvolt>(a.get(chip).data().r_g1v().as_microvolts() as f32),
                            GpioId::Gpio2 => Voltage::new::<microvolt>(a.get(chip).data().r_g2v().as_microvolts() as f32),
                            GpioId::Gpio3 => Voltage::new::<microvolt>(a.get(chip).data().r_g3v().as_microvolts() as f32),

                            GpioId::Gpio4 => Voltage::new::<microvolt>(b.get(chip).data().r_g4v().as_microvolts() as f32),
                            GpioId::Gpio5 => Voltage::new::<microvolt>(b.get(chip).data().r_g5v().as_microvolts() as f32),
                            GpioId::Gpio6 => Voltage::new::<microvolt>(b.get(chip).data().r_g6v().as_microvolts() as f32),

                            GpioId::Gpio7 => Voltage::new::<microvolt>(c.get(chip).data().r_g7v().as_microvolts() as f32),
                            GpioId::Gpio8 => Voltage::new::<microvolt>(c.get(chip).data().r_g8v().as_microvolts() as f32),
                            GpioId::Gpio9 => Voltage::new::<microvolt>(c.get(chip).data().r_g9v().as_microvolts() as f32),

                            GpioId::Gpio10 => Voltage::new::<microvolt>(d.get(chip).data().r_g10v().as_microvolts() as f32),
                        }),
                    })
                },
            })
        }
    }

    impl CacheData {
        /// Updates caches RedundantAuxillaryA through D with new data.
        ///
        /// This doesn't run the `autoconvert` function!! The caller should do that. Otherwise this update will basically do nothing
        ///
        /// ### Returns
        /// Will return `Ok(())`, or `Err(UpdateError)` if an error occurred. If this returns `Ok(())`, the cached data was updated correctly and can be read now.
        pub(in crate::segments) async fn update_redundant_aux(&self, api: &mut alias::Api) -> Result<(), UpdateError> {
            self.raxa.update(api).await?;
            self.raxb.update(api).await?;
            self.raxc.update(api).await?;
            self.raxd.update(api).await?;

            Ok(())
        }

        /// Gets the current cached Redundant Aux data.
        pub fn get_redundant_aux(&self) -> redundant_aux::Raw {
            redundant_aux::Raw {
                raxa: self.raxa.data(),
                raxb: self.raxb.data(),
                raxc: self.raxc.data(),
                raxd: self.raxd.data(),
            }
        }
    }
}

/// Register groups CellVoltages A through E (no F because we only use 13 cells).
pub mod cell_voltages {
    use super::*;
    use crate::units::Voltage;
    use uom::si::{electric_potential::microvolt};
    use super::alias;
    use crate::segments::chips::cells::{IndexByCell, CellId};

    /// Raw CellVoltages register readings.
    pub struct Raw {
        pub cva: RegisterCacheData<CellVoltagesA>,
        pub cvb: RegisterCacheData<CellVoltagesB>,
        pub cvc: RegisterCacheData<CellVoltagesC>,
        pub cvd: RegisterCacheData<CellVoltagesD>,
        pub cve: RegisterCacheData<CellVoltagesE>,
    }
    impl Raw {
        /// Tries to make it nice.
        ///
        /// If no data has been read to that cache yet, this will return `Err(())`.
        pub fn try_nice(&self) -> Result<NiceData, ()> {
            NiceData::try_from(self)
        }
    }
    // ^^ note: this struct is just meant to be a nice helper for formatting returned data. the `CacheData` struct is still meant to directly hold these registers itself

    /// "Nice data" for a single chip.
    pub struct NiceDataChip {
        inner: IndexByCell<Voltage>,
    }
    impl core::ops::Deref for NiceDataChip {
        type Target = IndexByCell<Voltage>;

        fn deref(&self) -> &Self::Target {
            &self.inner
        }
    }

    /// Represents the raw register readings, but formatted in a more readable way.
    ///
    /// This doesn't contain any metadata about the reading (e.g., PEC errors). So you should
    /// probably inspect that stuff from the `Raw` readings before converting to this.
    pub struct NiceData {
        inner: IndexByChip<NiceDataChip>,
    }
    impl core::ops::Deref for NiceData {
        type Target = IndexByChip<NiceDataChip>;

        fn deref(&self) -> &Self::Target {
            &self.inner
        }
    }
    impl TryFrom<&Raw> for NiceData {
        type Error = ();

        /// Attempts to convert `Raw` data into a `NiceData`. If any of the registers involved
        /// in this `NiceData` haven't been read yet, this returns `Err(())`.
        fn try_from(raw: &Raw) -> Result<Self, Self::Error> {
            let Some(a) = raw.cva.data() else {
                return Err(());
            };
            let Some(b) = raw.cvb.data() else {
                return Err(());
            };
            let Some(c) = raw.cvc.data() else {
                return Err(());
            };
            let Some(d) = raw.cvd.data() else {
                return Err(());
            };
            let Some(e) = raw.cve.data() else {
                return Err(());
            };

            Ok(Self {
                inner: {
                    IndexByChip::from_fn(|chip| NiceDataChip {
                        inner: IndexByCell::from_fn(|cell| match cell {
                            CellId::Cell1 => Voltage::new::<microvolt>(a.get(chip).data().c1v().as_microvolts() as f32),
                            CellId::Cell2 => Voltage::new::<microvolt>(a.get(chip).data().c2v().as_microvolts() as f32),
                            CellId::Cell3 => Voltage::new::<microvolt>(a.get(chip).data().c3v().as_microvolts() as f32),

                            CellId::Cell4 => Voltage::new::<microvolt>(b.get(chip).data().c4v().as_microvolts() as f32),
                            CellId::Cell5 => Voltage::new::<microvolt>(b.get(chip).data().c5v().as_microvolts() as f32),
                            CellId::Cell6 => Voltage::new::<microvolt>(b.get(chip).data().c6v().as_microvolts() as f32),

                            CellId::Cell7 => Voltage::new::<microvolt>(c.get(chip).data().c7v().as_microvolts() as f32),
                            CellId::Cell8 => Voltage::new::<microvolt>(c.get(chip).data().c8v().as_microvolts() as f32),
                            CellId::Cell9 => Voltage::new::<microvolt>(c.get(chip).data().c9v().as_microvolts() as f32),

                            CellId::Cell10 => Voltage::new::<microvolt>(d.get(chip).data().c10v().as_microvolts() as f32),
                            CellId::Cell11 => Voltage::new::<microvolt>(d.get(chip).data().c11v().as_microvolts() as f32),
                            CellId::Cell12 => Voltage::new::<microvolt>(d.get(chip).data().c12v().as_microvolts() as f32),

                            CellId::Cell13 => Voltage::new::<microvolt>(e.get(chip).data().c13v().as_microvolts() as f32),
                        }),
                    })
                },
            })
        }
    }

    impl CacheData {
        /// Updates caches CellVoltages A through E with new data.
        ///
        /// ### Returns
        /// Will return `Ok(())`, or `Err(UpdateError)` if an error occurred. If this returns `Ok(())`, the cached data was updated correctly and can be read now.
        pub(in crate::segments) async fn update_cell_voltages(&self, api: &mut alias::Api) -> Result<(), UpdateError> {
            let result: Result<(), UpdateError> = async {
                self.cva.update(api).await?;
                self.cvb.update(api).await?;
                self.cvc.update(api).await?;
                self.cvd.update(api).await?;
                self.cve.update(api).await?;
                Ok(())
            }
            .await;

            result
        }

        /// Gets the current cached Cell Voltages data.
        pub fn get_cell_voltages(&self) -> cell_voltages::Raw {
            cell_voltages::Raw {
                cva: self.cva.data(),
                cvb: self.cvb.data(),
                cvc: self.cvc.data(),
                cvd: self.cvd.data(),
                cve: self.cve.data(),
            }
        }
    }
}

/// Register groups AverageCellVoltages A through E (no F because we only use 13 cells).
pub mod average_cell_voltages {
    use super::*;
    use crate::units::Voltage;
    use uom::si::{electric_potential::microvolt};
    use super::alias;
    use crate::segments::chips::cells::{IndexByCell, CellId};

    /// Raw AverageCellVoltages register readings.
    pub struct Raw {
        pub aca: RegisterCacheData<AverageCellVoltagesA>,
        pub acb: RegisterCacheData<AverageCellVoltagesB>,
        pub acc: RegisterCacheData<AverageCellVoltagesC>,
        pub acd: RegisterCacheData<AverageCellVoltagesD>,
        pub ace: RegisterCacheData<AverageCellVoltagesE>,
    }
    impl Raw {
        /// Tries to make it nice.
        ///
        /// If no data has been read to that cache yet, this will return `Err(())`.
        pub fn try_nice(&self) -> Result<NiceData, ()> {
            NiceData::try_from(self)
        }
    }
    // ^^ note: this struct is just meant to be a nice helper for formatting returned data. the `CacheData` struct is still meant to directly hold these registers itself

    /// "Nice data" for a single chip.
    pub struct NiceDataChip {
        inner: IndexByCell<Voltage>,
    }
    impl core::ops::Deref for NiceDataChip {
        type Target = IndexByCell<Voltage>;

        fn deref(&self) -> &Self::Target {
            &self.inner
        }
    }

    /// Represents the raw register readings, but formatted in a more readable way.
    ///
    /// This doesn't contain any metadata about the reading (e.g., PEC errors). So you should
    /// probably inspect that stuff from the `Raw` readings before converting to this.
    pub struct NiceData {
        inner: IndexByChip<NiceDataChip>,
    }
    impl core::ops::Deref for NiceData {
        type Target = IndexByChip<NiceDataChip>;

        fn deref(&self) -> &Self::Target {
            &self.inner
        }
    }
    impl TryFrom<&Raw> for NiceData {
        type Error = ();

        /// Attempts to convert `Raw` data into a `NiceData`. If any of the registers involved
        /// in this `NiceData` haven't been read yet, this returns `Err(())`.
        fn try_from(raw: &Raw) -> Result<Self, Self::Error> {
            let Some(a) = raw.aca.data() else {
                return Err(());
            };
            let Some(b) = raw.acb.data() else {
                return Err(());
            };
            let Some(c) = raw.acc.data() else {
                return Err(());
            };
            let Some(d) = raw.acd.data() else {
                return Err(());
            };
            let Some(e) = raw.ace.data() else {
                return Err(());
            };

            Ok(Self {
                inner: {
                    IndexByChip::from_fn(|chip| NiceDataChip {
                        inner: IndexByCell::from_fn(|cell| match cell {
                            CellId::Cell1 => Voltage::new::<microvolt>(a.get(chip).data().ac1v().as_microvolts() as f32),
                            CellId::Cell2 => Voltage::new::<microvolt>(a.get(chip).data().ac2v().as_microvolts() as f32),
                            CellId::Cell3 => Voltage::new::<microvolt>(a.get(chip).data().ac3v().as_microvolts() as f32),

                            CellId::Cell4 => Voltage::new::<microvolt>(b.get(chip).data().ac4v().as_microvolts() as f32),
                            CellId::Cell5 => Voltage::new::<microvolt>(b.get(chip).data().ac5v().as_microvolts() as f32),
                            CellId::Cell6 => Voltage::new::<microvolt>(b.get(chip).data().ac6v().as_microvolts() as f32),

                            CellId::Cell7 => Voltage::new::<microvolt>(c.get(chip).data().ac7v().as_microvolts() as f32),
                            CellId::Cell8 => Voltage::new::<microvolt>(c.get(chip).data().ac8v().as_microvolts() as f32),
                            CellId::Cell9 => Voltage::new::<microvolt>(c.get(chip).data().ac9v().as_microvolts() as f32),

                            CellId::Cell10 => Voltage::new::<microvolt>(d.get(chip).data().ac10v().as_microvolts() as f32),
                            CellId::Cell11 => Voltage::new::<microvolt>(d.get(chip).data().ac11v().as_microvolts() as f32),
                            CellId::Cell12 => Voltage::new::<microvolt>(d.get(chip).data().ac12v().as_microvolts() as f32),

                            CellId::Cell13 => Voltage::new::<microvolt>(e.get(chip).data().ac13v().as_microvolts() as f32),
                        }),
                    })
                },
            })
        }
    }

    impl CacheData {
        /// Updates caches AverageCellVoltages A through E with new data.
        ///
        /// ### Returns
        /// Will return `Ok(())`, or `Err(UpdateError)` if an error occurred. If this returns `Ok(())`, the cached data was updated correctly and can be read now.
        pub(in crate::segments) async fn update_average_cell_voltages(&self, api: &mut alias::Api) -> Result<(), UpdateError> {
            let result: Result<(), UpdateError> = async {
                self.aca.update(api).await?;
                self.acb.update(api).await?;
                self.acc.update(api).await?;
                self.acd.update(api).await?;
                self.ace.update(api).await?;
                Ok(())
            }
            .await;

            result
        }

        /// Gets the current cached Average Cell Voltages data.
        pub fn get_average_cell_voltages(&self) -> average_cell_voltages::Raw {
            average_cell_voltages::Raw {
                aca: self.aca.data(),
                acb: self.acb.data(),
                acc: self.acc.data(),
                acd: self.acd.data(),
                ace: self.ace.data(),
            }
        }
    }
}

/// Register groups FilteredCellVoltages A through E (no F because we only use 13 cells).
pub mod filtered_cell_voltages {
    use super::*;
    use crate::units::Voltage;
    use uom::si::{electric_potential::microvolt};
    use super::alias;
    use crate::segments::chips::cells::{IndexByCell, CellId};

    /// Raw FilteredCellVoltages register readings.
    pub struct Raw {
        pub fca: RegisterCacheData<FilteredCellVoltagesA>,
        pub fcb: RegisterCacheData<FilteredCellVoltagesB>,
        pub fcc: RegisterCacheData<FilteredCellVoltagesC>,
        pub fcd: RegisterCacheData<FilteredCellVoltagesD>,
        pub fce: RegisterCacheData<FilteredCellVoltagesE>,
    }
    impl Raw {
        /// Tries to make it nice.
        ///
        /// If no data has been read to that cache yet, this will return `Err(())`.
        pub fn try_nice(&self) -> Result<NiceData, ()> {
            NiceData::try_from(self)
        }
    }
    // ^^ note: this struct is just meant to be a nice helper for formatting returned data. the `CacheData` struct is still meant to directly hold these registers itself

    /// "Nice data" for a single chip.
    pub struct NiceDataChip {
        inner: IndexByCell<Voltage>,
    }
    impl NiceDataChip {
        pub const fn cells(&self) -> &IndexByCell<Voltage> {
            &self.inner
        }
    }
    impl core::ops::Deref for NiceDataChip {
        type Target = IndexByCell<Voltage>;

        fn deref(&self) -> &Self::Target {
            &self.inner
        }
    }

    /// Represents the raw register readings, but formatted in a more readable way.
    ///
    /// This doesn't contain any metadata about the reading (e.g., PEC errors). So you should
    /// probably inspect that stuff from the `Raw` readings before converting to this.
    pub struct NiceData {
        inner: IndexByChip<NiceDataChip>,
    }
    impl core::ops::Deref for NiceData {
        type Target = IndexByChip<NiceDataChip>;

        fn deref(&self) -> &Self::Target {
            &self.inner
        }
    }
    impl TryFrom<&Raw> for NiceData {
        type Error = ();

        /// Attempts to convert `Raw` data into a `NiceData`. If any of the registers involved
        /// in this `NiceData` haven't been read yet, this returns `Err(())`.
        fn try_from(raw: &Raw) -> Result<Self, Self::Error> {
            let Some(a) = raw.fca.data() else {
                return Err(());
            };
            let Some(b) = raw.fcb.data() else {
                return Err(());
            };
            let Some(c) = raw.fcc.data() else {
                return Err(());
            };
            let Some(d) = raw.fcd.data() else {
                return Err(());
            };
            let Some(e) = raw.fce.data() else {
                return Err(());
            };

            Ok(Self {
                inner: {
                    IndexByChip::from_fn(|chip| NiceDataChip {
                        inner: IndexByCell::from_fn(|cell| match cell {
                            CellId::Cell1 => Voltage::new::<microvolt>(a.get(chip).data().fc1v().as_microvolts() as f32),
                            CellId::Cell2 => Voltage::new::<microvolt>(a.get(chip).data().fc2v().as_microvolts() as f32),
                            CellId::Cell3 => Voltage::new::<microvolt>(a.get(chip).data().fc3v().as_microvolts() as f32),

                            CellId::Cell4 => Voltage::new::<microvolt>(b.get(chip).data().fc4v().as_microvolts() as f32),
                            CellId::Cell5 => Voltage::new::<microvolt>(b.get(chip).data().fc5v().as_microvolts() as f32),
                            CellId::Cell6 => Voltage::new::<microvolt>(b.get(chip).data().fc6v().as_microvolts() as f32),

                            CellId::Cell7 => Voltage::new::<microvolt>(c.get(chip).data().fc7v().as_microvolts() as f32),
                            CellId::Cell8 => Voltage::new::<microvolt>(c.get(chip).data().fc8v().as_microvolts() as f32),
                            CellId::Cell9 => Voltage::new::<microvolt>(c.get(chip).data().fc9v().as_microvolts() as f32),

                            CellId::Cell10 => Voltage::new::<microvolt>(d.get(chip).data().fc10v().as_microvolts() as f32),
                            CellId::Cell11 => Voltage::new::<microvolt>(d.get(chip).data().fc11v().as_microvolts() as f32),
                            CellId::Cell12 => Voltage::new::<microvolt>(d.get(chip).data().fc12v().as_microvolts() as f32),

                            CellId::Cell13 => Voltage::new::<microvolt>(e.get(chip).data().fc13v().as_microvolts() as f32),
                        }),
                    })
                },
            })
        }
    }

    impl CacheData {
        /// Updates caches FilteredCellVoltages A through E with new data.
        ///
        /// ### Returns
        /// Will return `Ok(())`, or `Err(UpdateError)` if an error occurred. If this returns `Ok(())`, the cached data was updated correctly and can be read now.
        pub(in crate::segments) async fn update_filtered_cell_voltages(&self, api: &mut alias::Api) -> Result<(), UpdateError> {
            let result: Result<(), UpdateError> = async {
                self.fca.update(api).await?;
                self.fcb.update(api).await?;
                self.fcc.update(api).await?;
                self.fcd.update(api).await?;
                self.fce.update(api).await?;
                Ok(())
            }
            .await;

            result
        }

        /// Gets the current cached Filtered Cell Voltages data.
        pub fn get_filtered_cell_voltages(&self) -> filtered_cell_voltages::Raw {
            filtered_cell_voltages::Raw {
                fca: self.fca.data(),
                fcb: self.fcb.data(),
                fcc: self.fcc.data(),
                fcd: self.fcd.data(),
                fce: self.fce.data(),
            }
        }
    }
}

/// Register groups SVoltages A through E (no F because we only use 13 cells).
pub mod s_voltages {
    use super::*;
    use crate::units::Voltage;
    use uom::si::{electric_potential::microvolt};
    use super::alias;
    use crate::segments::chips::cells::{IndexByCell, CellId};

    /// Raw SVoltages register readings.
    pub struct Raw {
        pub sca: RegisterCacheData<SVoltagesA>,
        pub scb: RegisterCacheData<SVoltagesB>,
        pub scc: RegisterCacheData<SVoltagesC>,
        pub scd: RegisterCacheData<SVoltagesD>,
        pub sce: RegisterCacheData<SVoltagesE>,
    }
    impl Raw {
        /// Tries to make it nice.
        ///
        /// If no data has been read to that cache yet, this will return `Err(())`.
        pub fn try_nice(&self) -> Result<NiceData, ()> {
            NiceData::try_from(self)
        }
    }
    // ^^ note: this struct is just meant to be a nice helper for formatting returned data. the `CacheData` struct is still meant to directly hold these registers itself

    /// "Nice data" for a single chip.
    pub struct NiceDataChip {
        inner: IndexByCell<Voltage>,
    }
    impl core::ops::Deref for NiceDataChip {
        type Target = IndexByCell<Voltage>;

        fn deref(&self) -> &Self::Target {
            &self.inner
        }
    }

    /// Represents the raw register readings, but formatted in a more readable way.
    ///
    /// This doesn't contain any metadata about the reading (e.g., PEC errors). So you should
    /// probably inspect that stuff from the `Raw` readings before converting to this.
    pub struct NiceData {
        inner: IndexByChip<NiceDataChip>,
    }
    impl core::ops::Deref for NiceData {
        type Target = IndexByChip<NiceDataChip>;

        fn deref(&self) -> &Self::Target {
            &self.inner
        }
    }
    impl TryFrom<&Raw> for NiceData {
        type Error = ();

        /// Attempts to convert `Raw` data into a `NiceData`. If any of the registers involved
        /// in this `NiceData` haven't been read yet, this returns `Err(())`.
        fn try_from(raw: &Raw) -> Result<Self, Self::Error> {
            let Some(a) = raw.sca.data() else {
                return Err(());
            };
            let Some(b) = raw.scb.data() else {
                return Err(());
            };
            let Some(c) = raw.scc.data() else {
                return Err(());
            };
            let Some(d) = raw.scd.data() else {
                return Err(());
            };
            let Some(e) = raw.sce.data() else {
                return Err(());
            };

            Ok(Self {
                inner: {
                    IndexByChip::from_fn(|chip| NiceDataChip {
                        inner: IndexByCell::from_fn(|cell| match cell {
                            CellId::Cell1 => Voltage::new::<microvolt>(a.get(chip).data().s1v().as_microvolts() as f32),
                            CellId::Cell2 => Voltage::new::<microvolt>(a.get(chip).data().s2v().as_microvolts() as f32),
                            CellId::Cell3 => Voltage::new::<microvolt>(a.get(chip).data().s3v().as_microvolts() as f32),

                            CellId::Cell4 => Voltage::new::<microvolt>(b.get(chip).data().s4v().as_microvolts() as f32),
                            CellId::Cell5 => Voltage::new::<microvolt>(b.get(chip).data().s5v().as_microvolts() as f32),
                            CellId::Cell6 => Voltage::new::<microvolt>(b.get(chip).data().s6v().as_microvolts() as f32),

                            CellId::Cell7 => Voltage::new::<microvolt>(c.get(chip).data().s7v().as_microvolts() as f32),
                            CellId::Cell8 => Voltage::new::<microvolt>(c.get(chip).data().s8v().as_microvolts() as f32),
                            CellId::Cell9 => Voltage::new::<microvolt>(c.get(chip).data().s9v().as_microvolts() as f32),

                            CellId::Cell10 => Voltage::new::<microvolt>(d.get(chip).data().s10v().as_microvolts() as f32),
                            CellId::Cell11 => Voltage::new::<microvolt>(d.get(chip).data().s11v().as_microvolts() as f32),
                            CellId::Cell12 => Voltage::new::<microvolt>(d.get(chip).data().s12v().as_microvolts() as f32),

                            CellId::Cell13 => Voltage::new::<microvolt>(e.get(chip).data().s13v().as_microvolts() as f32),
                        }),
                    })
                },
            })
        }
    }

    impl CacheData {
        /// Updates caches SVoltages A through E with new data.
        ///
        /// ### Returns
        /// Will return `Ok(())`, or `Err(UpdateError)` if an error occurred. If this returns `Ok(())`, the cached data was updated correctly and can be read now.
        pub(in crate::segments) async fn update_s_voltages(&self, api: &mut alias::Api) -> Result<(), UpdateError> {
            let result: Result<(), UpdateError> = async {
                self.sca.update(api).await?;
                self.scb.update(api).await?;
                self.scc.update(api).await?;
                self.scd.update(api).await?;
                self.sce.update(api).await?;
                Ok(())
            }
            .await;

            result
        }

        /// Gets the current cached S Voltages data.
        pub fn get_s_voltages(&self) -> s_voltages::Raw {
            s_voltages::Raw {
                sca: self.sca.data(),
                scb: self.scb.data(),
                scc: self.scc.data(),
                scd: self.scd.data(),
                sce: self.sce.data(),
            }
        }
    }
}

/// Status C register group.
pub mod status_c {
    use super::*;
    use super::alias;
    use crate::segments::chips::cells::{IndexByCell, CellId};
    use adbms6830b::chip::registers::status::types::c::{ComparisonFault, ConversionsCount, STrimMultipleError, STrimError, CTrimMultipleError, CTrimError, DigitalRailOvervoltage, DigitalRailUndervoltage, AnalogRailUndervoltage, AnalogRailOvervoltage, OscillatorCheck, TestModeDetection, ThermalShutdownStatus, SleepModeDetection, SpiFault, ComparisonActive, SupplyRailDelta, SupplyRailDeltaLatent};

    /// Raw StatusC register reading.
    pub struct Raw {
        pub statc: RegisterCacheData<StatusC>,
    }
    // ^^ note: this struct is just meant to be a nice helper for formatting returned data. the `CacheData` struct is still meant to directly hold these registers itself

    /// "Nice data" for a single chip.
    ///
    /// This data isn't even that "nice", since it is mostly just the raw StatusC data but with nicer ComparisonFault formatting (can be indexed by cells) and nicer
    /// conversions count formatting. Everything else is basically the same though.
    pub struct NiceDataChip {
        pub cell_channel_comparison_faults: IndexByCell<ComparisonFault>,
        pub conversions_count: ConversionsCount,
        pub s_trim_multiple_error: STrimMultipleError,
        pub s_trim_error: STrimError,
        pub c_trim_multiple_error: CTrimMultipleError,
        pub c_trim_error: CTrimError,
        pub digital_rail_undervoltage: DigitalRailUndervoltage,
        pub digital_rail_overvoltage: DigitalRailOvervoltage,
        pub analog_rail_undervoltage: AnalogRailUndervoltage,
        pub analog_rail_overvoltage: AnalogRailOvervoltage,
        pub oscillator_check: OscillatorCheck,
        pub test_mode_detection: TestModeDetection,
        pub thermal_shutdown_status: ThermalShutdownStatus,
        pub sleep_mode_detection: SleepModeDetection,
        pub spi_fault: SpiFault,
        pub comparison_active: ComparisonActive,
        pub supply_rail_delta: SupplyRailDelta,
        pub supply_rail_delta_latent: SupplyRailDeltaLatent,
    }
    impl Raw {
        /// Tries to make it nice.
        ///
        /// If no data has been read to that cache yet, this will return `Err(())`.
        pub fn try_nice(&self) -> Result<NiceData, ()> {
            NiceData::try_from(self)
        }
    }

    /// Represents the raw register readings, but formatted in a more readable way.
    ///
    /// This doesn't contain any metadata about the reading (e.g., PEC errors). So you should
    /// probably inspect that stuff from the `Raw` readings before converting to this.
    pub struct NiceData {
        inner: IndexByChip<NiceDataChip>,
    }
    impl core::ops::Deref for NiceData {
        type Target = IndexByChip<NiceDataChip>;

        fn deref(&self) -> &Self::Target {
            &self.inner
        }
    }
    impl TryFrom<&Raw> for NiceData {
        type Error = ();

        /// Attempts to convert `Raw` data into a `NiceData`. If any of the registers involved
        /// in this `NiceData` haven't been read yet, this returns `Err(())`.
        fn try_from(raw: &Raw) -> Result<Self, Self::Error> {
            let Some(statc) = raw.statc.data() else {
                return Err(());
            };

            Ok(Self {
                inner: {
                    IndexByChip::from_fn(|chip| {
                        let chip_statc = statc.get(chip).data();
                        NiceDataChip {
                            cell_channel_comparison_faults: IndexByCell::from_fn(|cell| match cell {
                                CellId::Cell1 => chip_statc.cs1flt(),
                                CellId::Cell2 => chip_statc.cs2flt(),
                                CellId::Cell3 => chip_statc.cs3flt(),
                                CellId::Cell4 => chip_statc.cs4flt(),
                                CellId::Cell5 => chip_statc.cs5flt(),
                                CellId::Cell6 => chip_statc.cs6flt(),
                                CellId::Cell7 => chip_statc.cs7flt(),
                                CellId::Cell8 => chip_statc.cs8flt(),
                                CellId::Cell9 => chip_statc.cs9flt(),
                                CellId::Cell10 => chip_statc.cs10flt(),
                                CellId::Cell11 => chip_statc.cs11flt(),
                                CellId::Cell12 => chip_statc.cs12flt(),
                                CellId::Cell13 => chip_statc.cs13flt(),
                            }),
                            conversions_count: ConversionsCount::new(chip_statc.ct_lower(), chip_statc.ct_upper(), chip_statc.cts()),
                            s_trim_multiple_error: chip_statc.smed(),
                            s_trim_error: chip_statc.sed(),
                            c_trim_multiple_error: chip_statc.cmed(),
                            c_trim_error: chip_statc.ced(),
                            digital_rail_overvoltage: chip_statc.vd_ov(),
                            digital_rail_undervoltage: chip_statc.vd_uv(),
                            analog_rail_overvoltage: chip_statc.va_ov(),
                            analog_rail_undervoltage: chip_statc.va_uv(),
                            oscillator_check: chip_statc.oscchk(),
                            test_mode_detection: chip_statc.tmodchk(),
                            thermal_shutdown_status: chip_statc.thsd(),
                            sleep_mode_detection: chip_statc.sleep(),
                            spi_fault: chip_statc.spiflt(),
                            comparison_active: chip_statc.comp(),
                            supply_rail_delta: chip_statc.vde(),
                            supply_rail_delta_latent: chip_statc.vdel(),
                        }
                    })
                },
            })
        }
    }

    impl CacheData {
        /// Helper that updates fault counters for StatusC counts, and then returns an array of the ClearFlags we should clear
        fn update_status_c_fault_counts(&self, readings: &IndexByChip<Reading<StatusC>>) -> IndexByChip<ClearFlags> {
            let mut clears: IndexByChip<ClearFlags> = IndexByChip::from_fn(|_| ClearFlags::new());

            self.fault_counts.lock(|cell| {
                let mut counts = cell.get();

                for (chip, reading) in readings.iter() {
                    // if the PEC failed then we shouldnt count any of those fault flags because they could just be junk. for the same reason, we dont want to W1C those flags either. if they are really set then they will appear when we have a read with a PEC that actually passes
                    if !reading.pec().is_success() {
                        continue;
                    }

                    let statc = reading.data();
                    let c = counts.get_mut(chip);
                    let clear = clears.get_mut(chip);

                    macro_rules! record {
                        ($flag:ident, $count:expr, $with:ident) => {
                            if statc.$flag().is_set() {
                                $count = $count.saturating_add(1);
                                *clear = clear.$with(ClearAction::Clear);
                            }
                        };
                    }

                    record!(cs1flt, c.csxflt.cs1flt, with_cl_cs1flt);
                    record!(cs2flt, c.csxflt.cs2flt, with_cl_cs2flt);
                    record!(cs3flt, c.csxflt.cs3flt, with_cl_cs3flt);
                    record!(cs4flt, c.csxflt.cs4flt, with_cl_cs4flt);
                    record!(cs5flt, c.csxflt.cs5flt, with_cl_cs5flt);
                    record!(cs6flt, c.csxflt.cs6flt, with_cl_cs6flt);
                    record!(cs7flt, c.csxflt.cs7flt, with_cl_cs7flt);
                    record!(cs8flt, c.csxflt.cs8flt, with_cl_cs8flt);
                    record!(cs9flt, c.csxflt.cs9flt, with_cl_cs9flt);
                    record!(cs10flt, c.csxflt.cs10flt, with_cl_cs10flt);
                    record!(cs11flt, c.csxflt.cs11flt, with_cl_cs11flt);
                    record!(cs12flt, c.csxflt.cs12flt, with_cl_cs12flt);
                    record!(cs13flt, c.csxflt.cs13flt, with_cl_cs13flt);
                    record!(cs14flt, c.csxflt.cs14flt, with_cl_cs14flt);
                    record!(cs15flt, c.csxflt.cs15flt, with_cl_cs15flt);
                    record!(cs16flt, c.csxflt.cs16flt, with_cl_cs16flt);

                    record!(smed, c.smed, with_cl_smed);
                    record!(sed, c.sed, with_cl_sed);
                    record!(cmed, c.cmed, with_cl_cmed);
                    record!(ced, c.ced, with_cl_ced);
                    record!(vd_uv, c.vd_uv, with_cl_vduv);
                    record!(vd_ov, c.vd_ov, with_cl_vdov);
                    record!(va_uv, c.va_uv, with_cl_vauv);
                    record!(va_ov, c.va_ov, with_cl_vaov);
                    record!(oscchk, c.oscchk, with_cl_oscchk);
                    record!(tmodchk, c.tmodchk, with_cl_tmode);
                    record!(thsd, c.thsd, with_cl_thsd);
                    record!(spiflt, c.spiflt, with_cl_spiflt);
                    record!(vde, c.vde, with_cl_vde);
                    record!(vdel, c.vdel, with_cl_vdel);

                    // SPECIFIC CASE FOR THE SLEEP BIT
                    // the SLEEP bit gets read and cleared by the `Service` as part of the sleep detection stuff.
                    // so we should add increment the flag's counter in the fault counter, but shouldnt clear it. this way, the `Service` will be able to read the SLEEP bit.
                    // this makes it so the Service can correctly detect sleep and flag the app so we can reset (which includes clearing the SLEEP bit).
                    //
                    // this is kinda jank. realistically the Service should probably be the one that owns all the flag clears/fault counting, or maybe the sleep detection stuff (and maybe isospi recovery) should just be moved into the application entirely.
                    // but for now this is the most straightforward solution
                    if statc.sleep().is_set() {
                        c.sleep = c.sleep.saturating_add(1);
                        // dont clear it!!!
                    }
                }

                cell.set(counts);
            });

            clears
        }

        /// Updates StatusC cache.
        ///
        /// ### Returns
        /// Will return `Ok(())`, or `Err(UpdateError)` if an error occurred. If this returns `Ok(())`, the cached data was updated correctly and can be read now.
        pub(in crate::segments) async fn update_status_c(&self, api: &mut alias::Api) -> Result<(), UpdateError> {
            let result: Result<(), UpdateError> = async {
                self.statc.update(api).await?;
                Ok(())
            }
            .await;

            if result.is_ok() {
                let statc_data = self.statc.data();
                if let Some(readings) = statc_data.data().as_ref() {
                    let clears = self.update_status_c_fault_counts(readings);

                    if let Err(err) = api.write(&clears.into_array()).await {
                        defmt::error!("Segments: Cache: in `update_status_c()`: ClearFlags write failed. Error: {}", err);
                        return Err(UpdateError::ClearFlagsError(err));
                    }
                }
            }

            result
        }

        /// Gets the current cached StatusC data.
        pub fn get_status_c(&self) -> status_c::Raw {
            status_c::Raw { statc: self.statc.data() }
        }
    }
}

/// Status D register group.
pub mod status_d {
    use super::*;
    use super::alias;
    use crate::segments::chips::cells::{IndexByCell, CellId};
    use adbms6830b::chip::registers::status::types::d::{CellUndervoltageFlag, CellOvervoltageFlag, OscillatorCheckCounter};

    /// Raw StatusD register reading.
    pub struct Raw {
        pub statd: RegisterCacheData<StatusD>,
    }
    // ^^ note: this struct is just meant to be a nice helper for formatting returned data. the `CacheData` struct is still meant to directly hold these registers itself

    /// Undervoltage/overvoltage flag state for an individual cell.
    pub struct CellUndervoltageOvervoltageState {
        pub undervoltage: CellUndervoltageFlag,
        pub overvoltage: CellOvervoltageFlag,
    }

    /// "Nice data" for a single chip.
    pub struct NiceDataChip {
        pub cell_undervoltage_overvoltage_state: IndexByCell<CellUndervoltageOvervoltageState>,
        pub oscillator_check_counter: OscillatorCheckCounter,
    }
    impl Raw {
        /// Tries to make it nice.
        ///
        /// If no data has been read to that cache yet, this will return `Err(())`.
        pub fn try_nice(&self) -> Result<NiceData, ()> {
            NiceData::try_from(self)
        }
    }

    /// Represents the raw register readings, but formatted in a more readable way.
    ///
    /// This doesn't contain any metadata about the reading (e.g., PEC errors). So you should
    /// probably inspect that stuff from the `Raw` readings before converting to this.
    pub struct NiceData {
        inner: IndexByChip<NiceDataChip>,
    }
    impl core::ops::Deref for NiceData {
        type Target = IndexByChip<NiceDataChip>;

        fn deref(&self) -> &Self::Target {
            &self.inner
        }
    }
    impl TryFrom<&Raw> for NiceData {
        type Error = ();

        /// Attempts to convert `Raw` data into a `NiceData`. If any of the registers involved
        /// in this `NiceData` haven't been read yet, this returns `Err(())`.
        fn try_from(raw: &Raw) -> Result<Self, Self::Error> {
            let Some(statd) = raw.statd.data() else {
                return Err(());
            };

            Ok(Self {
                inner: {
                    IndexByChip::from_fn(|chip| {
                        let chip_statd = statd.get(chip).data();
                        NiceDataChip {
                            cell_undervoltage_overvoltage_state: IndexByCell::from_fn(|cell| match cell {
                                CellId::Cell1 => CellUndervoltageOvervoltageState { undervoltage: chip_statd.c1uv(), overvoltage: chip_statd.c1ov() },
                                CellId::Cell2 => CellUndervoltageOvervoltageState { undervoltage: chip_statd.c2uv(), overvoltage: chip_statd.c2ov() },
                                CellId::Cell3 => CellUndervoltageOvervoltageState { undervoltage: chip_statd.c3uv(), overvoltage: chip_statd.c3ov() },
                                CellId::Cell4 => CellUndervoltageOvervoltageState { undervoltage: chip_statd.c4uv(), overvoltage: chip_statd.c4ov() },
                                CellId::Cell5 => CellUndervoltageOvervoltageState { undervoltage: chip_statd.c5uv(), overvoltage: chip_statd.c5ov() },
                                CellId::Cell6 => CellUndervoltageOvervoltageState { undervoltage: chip_statd.c6uv(), overvoltage: chip_statd.c6ov() },
                                CellId::Cell7 => CellUndervoltageOvervoltageState { undervoltage: chip_statd.c7uv(), overvoltage: chip_statd.c7ov() },
                                CellId::Cell8 => CellUndervoltageOvervoltageState { undervoltage: chip_statd.c8uv(), overvoltage: chip_statd.c8ov() },
                                CellId::Cell9 => CellUndervoltageOvervoltageState { undervoltage: chip_statd.c9uv(), overvoltage: chip_statd.c9ov() },
                                CellId::Cell10 => CellUndervoltageOvervoltageState { undervoltage: chip_statd.c10uv(), overvoltage: chip_statd.c10ov() },
                                CellId::Cell11 => CellUndervoltageOvervoltageState { undervoltage: chip_statd.c11uv(), overvoltage: chip_statd.c11ov() },
                                CellId::Cell12 => CellUndervoltageOvervoltageState { undervoltage: chip_statd.c12uv(), overvoltage: chip_statd.c12ov() },
                                CellId::Cell13 => CellUndervoltageOvervoltageState { undervoltage: chip_statd.c13uv(), overvoltage: chip_statd.c13ov() },
                            }),
                            oscillator_check_counter: chip_statd.oc_cntr(),
                        }
                    })
                },
            })
        }
    }

    impl CacheData {
        /// Helper that updates fault counters for StatusD counts, and then returns an array of the ClearOvervoltageUndervoltage we should clear
        fn update_status_d_fault_counts(&self, readings: &IndexByChip<Reading<StatusD>>) -> IndexByChip<ClearOvervoltageUndervoltage> {
            let mut clears: IndexByChip<ClearOvervoltageUndervoltage> = IndexByChip::from_fn(|_| ClearOvervoltageUndervoltage::new());

            self.fault_counts.lock(|cell| {
                let mut counts = cell.get();

                for (chip, reading) in readings.iter() {
                    // if the PEC failed then we shouldnt count any of those fault flags because they could just be junk. for the same reason, we dont want to W1C those flags either. if they are really set then they will appear when we have a read with a PEC that actually passes
                    if !reading.pec().is_success() {
                        continue;
                    }

                    let statd = reading.data();
                    let c = counts.get_mut(chip);
                    let clear = clears.get_mut(chip);

                    macro_rules! record {
                        ($flag:ident, $count:expr, $with:ident) => {
                            if statd.$flag().is_set() {
                                $count = $count.saturating_add(1);
                                *clear = clear.$with(ClearAction::Clear);
                            }
                        };
                    }

                    record!(c1uv, c.cxovuv.c1uv, with_cl_c1uv);
                    record!(c1ov, c.cxovuv.c1ov, with_cl_c1ov);
                    record!(c2uv, c.cxovuv.c2uv, with_cl_c2uv);
                    record!(c2ov, c.cxovuv.c2ov, with_cl_c2ov);
                    record!(c3uv, c.cxovuv.c3uv, with_cl_c3uv);
                    record!(c3ov, c.cxovuv.c3ov, with_cl_c3ov);
                    record!(c4uv, c.cxovuv.c4uv, with_cl_c4uv);
                    record!(c4ov, c.cxovuv.c4ov, with_cl_c4ov);
                    record!(c5uv, c.cxovuv.c5uv, with_cl_c5uv);
                    record!(c5ov, c.cxovuv.c5ov, with_cl_c5ov);
                    record!(c6uv, c.cxovuv.c6uv, with_cl_c6uv);
                    record!(c6ov, c.cxovuv.c6ov, with_cl_c6ov);
                    record!(c7uv, c.cxovuv.c7uv, with_cl_c7uv);
                    record!(c7ov, c.cxovuv.c7ov, with_cl_c7ov);
                    record!(c8uv, c.cxovuv.c8uv, with_cl_c8uv);
                    record!(c8ov, c.cxovuv.c8ov, with_cl_c8ov);
                    record!(c9uv, c.cxovuv.c9uv, with_cl_c9uv);
                    record!(c9ov, c.cxovuv.c9ov, with_cl_c9ov);
                    record!(c10uv, c.cxovuv.c10uv, with_cl_c10uv);
                    record!(c10ov, c.cxovuv.c10ov, with_cl_c10ov);
                    record!(c11uv, c.cxovuv.c11uv, with_cl_c11uv);
                    record!(c11ov, c.cxovuv.c11ov, with_cl_c11ov);
                    record!(c12uv, c.cxovuv.c12uv, with_cl_c12uv);
                    record!(c12ov, c.cxovuv.c12ov, with_cl_c12ov);
                    record!(c13uv, c.cxovuv.c13uv, with_cl_c13uv);
                    record!(c13ov, c.cxovuv.c13ov, with_cl_c13ov);
                    record!(c14uv, c.cxovuv.c14uv, with_cl_c14uv);
                    record!(c14ov, c.cxovuv.c14ov, with_cl_c14ov);
                    record!(c15uv, c.cxovuv.c15uv, with_cl_c15uv);
                    record!(c15ov, c.cxovuv.c15ov, with_cl_c15ov);
                    record!(c16uv, c.cxovuv.c16uv, with_cl_c16uv);
                    record!(c16ov, c.cxovuv.c16ov, with_cl_c16ov);
                }

                cell.set(counts);
            });

            clears
        }

        /// Updates StatusD cache.
        ///
        /// ### Returns
        /// Will return `Ok(())`, or `Err(UpdateError)` if an error occurred. If this returns `Ok(())`, the cached data was updated correctly and can be read now.
        pub(in crate::segments) async fn update_status_d(&self, api: &mut alias::Api) -> Result<(), UpdateError> {
            let result: Result<(), UpdateError> = async {
                self.statd.update(api).await?;
                Ok(())
            }
            .await;

            if result.is_ok() {
                let statd_data = self.statd.data();
                if let Some(readings) = statd_data.data().as_ref() {
                    let clears = self.update_status_d_fault_counts(readings);

                    if let Err(err) = api.write(&clears.into_array()).await {
                        defmt::error!("Segments: Cache: in `update_status_d()`: ClearOvervoltageUndervoltage write failed. Error: {}", err);
                        return Err(UpdateError::ClearFlagsError(err));
                    }
                }
            }

            result
        }

        /// Gets the current cached StatusD data.
        pub fn get_status_d(&self) -> status_d::Raw {
            status_d::Raw { statd: self.statd.data() }
        }
    }
}

/// Register groups AuxillaryA through D.
pub mod aux {

    use super::*;
    use crate::units::Voltage;
    use uom::si::{electric_potential::microvolt};
    use super::alias;
    use crate::segments::chips::gpios::{IndexByGpio, GpioId};

    /// Raw Aux register readings.
    pub struct Raw {
        pub auxa: RegisterCacheData<AuxillaryA>,
        pub auxb: RegisterCacheData<AuxillaryB>,
        pub auxc: RegisterCacheData<AuxillaryC>,
        pub auxd: RegisterCacheData<AuxillaryD>,
    }
    impl Raw {
        /// Tries to make it nice.
        ///
        /// If no data has been read to that cache yet, this will return `Err(())`.
        pub fn try_nice(&self) -> Result<NiceData, ()> {
            NiceData::try_from(self)
        }
    }
    // ^^ note: this struct is just meant to be a nice helper for formatting returned data. the `CacheData` struct is still meant to directly hold these registers itself

    /// "Nice data" for a single chip.
    pub struct NiceDataChip {
        /// GPIO voltages.
        pub gpio_voltages: IndexByGpio<Voltage>,
        /// VMV voltage result.
        pub vmv: Voltage,
        /// VPV voltage result.
        pub vpv: Voltage,
    }

    /// Represents the raw register readings, but formatted in a more readable way.
    ///
    /// This doesn't contain any metadata about the reading (e.g., PEC errors). So you should
    /// probably inspect that stuff from the `Raw` readings before converting to this.
    pub struct NiceData {
        inner: IndexByChip<NiceDataChip>,
    }
    impl core::ops::Deref for NiceData {
        type Target = IndexByChip<NiceDataChip>;

        fn deref(&self) -> &Self::Target {
            &self.inner
        }
    }
    impl TryFrom<&Raw> for NiceData {
        type Error = ();

        /// Attempts to convert `Raw` data into a `NiceData`. If any of the registers involved
        /// in this `NiceData` haven't been read yet, this returns `Err(())`.
        fn try_from(raw: &Raw) -> Result<Self, Self::Error> {
            let Some(a) = raw.auxa.data() else {
                return Err(());
            };
            let Some(b) = raw.auxb.data() else {
                return Err(());
            };
            let Some(c) = raw.auxc.data() else {
                return Err(());
            };
            let Some(d) = raw.auxd.data() else {
                return Err(());
            };

            Ok(Self {
                inner: {
                    IndexByChip::from_fn(|chip| NiceDataChip {
                        gpio_voltages: IndexByGpio::from_fn(|gpio| match gpio {
                            GpioId::Gpio1 => Voltage::new::<microvolt>(a.get(chip).data().g1v().as_microvolts() as f32),
                            GpioId::Gpio2 => Voltage::new::<microvolt>(a.get(chip).data().g2v().as_microvolts() as f32),
                            GpioId::Gpio3 => Voltage::new::<microvolt>(a.get(chip).data().g3v().as_microvolts() as f32),

                            GpioId::Gpio4 => Voltage::new::<microvolt>(b.get(chip).data().g4v().as_microvolts() as f32),
                            GpioId::Gpio5 => Voltage::new::<microvolt>(b.get(chip).data().g5v().as_microvolts() as f32),
                            GpioId::Gpio6 => Voltage::new::<microvolt>(b.get(chip).data().g6v().as_microvolts() as f32),

                            GpioId::Gpio7 => Voltage::new::<microvolt>(c.get(chip).data().g7v().as_microvolts() as f32),
                            GpioId::Gpio8 => Voltage::new::<microvolt>(c.get(chip).data().g8v().as_microvolts() as f32),
                            GpioId::Gpio9 => Voltage::new::<microvolt>(c.get(chip).data().g9v().as_microvolts() as f32),

                            GpioId::Gpio10 => Voltage::new::<microvolt>(d.get(chip).data().g10v().as_microvolts() as f32),
                        }),
                        vmv: Voltage::new::<microvolt>(d.get(chip).data().vmv().as_microvolts() as f32),
                        vpv: Voltage::new::<microvolt>(d.get(chip).data().vpv().as_microvolts() as f32),
                    })
                },
            })
        }
    }

    impl CacheData {
        /// Updates caches AuxillaryA through D with new data.
        ///
        /// This doesn't run the `autoconvert` function!! The caller should do that. Otherwise this update will basically do nothing
        ///
        /// ### Returns
        /// Will return `Ok(())`, or `Err(UpdateError)` if an error occurred. If this returns `Ok(())`, the cached data was updated correctly and can be read now.
        pub(in crate::segments) async fn update_aux(&self, api: &mut alias::Api) -> Result<(), UpdateError> {
            self.auxa.update(api).await?;
            self.auxb.update(api).await?;
            self.auxc.update(api).await?;
            self.auxd.update(api).await?;

            Ok(())
        }

        /// Gets the current cached Aux data.
        pub fn get_aux(&self) -> aux::Raw {
            aux::Raw {
                auxa: self.auxa.data(),
                auxb: self.auxb.data(),
                auxc: self.auxc.data(),
                auxd: self.auxd.data(),
            }
        }
    }
}

/// StatusA register group.
pub mod status_a {
    use super::*;
    use crate::units::{Voltage, Temperature, microcelcius};
    use uom::si::{electric_potential::microvolt};
    use super::alias;

    /// Raw StatusA register reading.
    pub struct Raw {
        pub stata: RegisterCacheData<StatusA>,
    }
    impl Raw {
        /// Tries to make it nice.
        ///
        /// If no data has been read to that cache yet, this will return `Err(())`.
        pub fn try_nice(&self) -> Result<NiceData, ()> {
            NiceData::try_from(self)
        }
    }
    // ^^ note: this struct is just meant to be a nice helper for formatting returned data. the `CacheData` struct is still meant to directly hold these registers itself

    /// "Nice data" for a single chip.
    pub struct NiceDataChip {
        /// Second reference voltage.
        pub vref2: Voltage,
        /// Internal die temperature.
        pub itmp: Temperature,
    }

    /// Represents the raw register readings, but formatted in a more readable way.
    ///
    /// This doesn't contain any metadata about the reading (e.g., PEC errors). So you should
    /// probably inspect that stuff from the `Raw` readings before converting to this.
    pub struct NiceData {
        inner: IndexByChip<NiceDataChip>,
    }
    impl core::ops::Deref for NiceData {
        type Target = IndexByChip<NiceDataChip>;

        fn deref(&self) -> &Self::Target {
            &self.inner
        }
    }
    impl TryFrom<&Raw> for NiceData {
        type Error = ();

        /// Attempts to convert `Raw` data into a `NiceData`. If any of the registers involved
        /// in this `NiceData` haven't been read yet, this returns `Err(())`.
        fn try_from(raw: &Raw) -> Result<Self, Self::Error> {
            let Some(stata) = raw.stata.data() else {
                return Err(());
            };

            Ok(Self {
                inner: {
                    IndexByChip::from_fn(|chip| NiceDataChip {
                        vref2: Voltage::new::<microvolt>(stata.get(chip).data().vref2().as_microvolts() as f32),
                        itmp: Temperature::new::<microcelcius>(stata.get(chip).data().itmp().as_microcelsius() as f32),
                    })
                },
            })
        }
    }

    impl CacheData {
        /// Updates StatusA cache with new data.
        ///
        /// This doesn't run the `autoconvert` function!! The caller should do that. Otherwise this update will basically do nothing
        ///
        /// ### Returns
        /// Will return `Ok(())`, or `Err(UpdateError)` if an error occurred. If this returns `Ok(())`, the cached data was updated correctly and can be read now.
        pub(in crate::segments) async fn update_status_a(&self, api: &mut alias::Api) -> Result<(), UpdateError> {
            self.stata.update(api).await?;

            Ok(())
        }

        /// Gets the current cached StatusA data.
        pub fn get_status_a(&self) -> status_a::Raw {
            status_a::Raw { stata: self.stata.data() }
        }
    }
}

/// StatusB register group.
pub mod status_b {
    use super::*;
    use crate::units::{Voltage};
    use uom::si::{electric_potential::microvolt};
    use super::alias;

    /// Raw StatusB register reading.
    pub struct Raw {
        pub statb: RegisterCacheData<StatusB>,
    }
    impl Raw {
        /// Tries to make it nice.
        ///
        /// If no data has been read to that cache yet, this will return `Err(())`.
        pub fn try_nice(&self) -> Result<NiceData, ()> {
            NiceData::try_from(self)
        }
    }
    // ^^ note: this struct is just meant to be a nice helper for formatting returned data. the `CacheData` struct is still meant to directly hold these registers itself

    /// "Nice data" for a single chip.
    pub struct NiceDataChip {
        /// Digital power supply voltage.
        pub vd: Voltage,
        /// Analog power supply voltage.
        pub va: Voltage,
        /// VREF2 voltage across resistor.
        pub vres: Voltage,
    }

    /// Represents the raw register readings, but formatted in a more readable way.
    ///
    /// This doesn't contain any metadata about the reading (e.g., PEC errors). So you should
    /// probably inspect that stuff from the `Raw` readings before converting to this.
    pub struct NiceData {
        inner: IndexByChip<NiceDataChip>,
    }
    impl core::ops::Deref for NiceData {
        type Target = IndexByChip<NiceDataChip>;

        fn deref(&self) -> &Self::Target {
            &self.inner
        }
    }
    impl TryFrom<&Raw> for NiceData {
        type Error = ();

        /// Attempts to convert `Raw` data into a `NiceData`. If any of the registers involved
        /// in this `NiceData` haven't been read yet, this returns `Err(())`.
        fn try_from(raw: &Raw) -> Result<Self, Self::Error> {
            let Some(statb) = raw.statb.data() else {
                return Err(());
            };

            Ok(Self {
                inner: {
                    IndexByChip::from_fn(|chip| NiceDataChip {
                        vd: Voltage::new::<microvolt>(statb.get(chip).data().vd().as_microvolts() as f32),
                        va: Voltage::new::<microvolt>(statb.get(chip).data().va().as_microvolts() as f32),
                        vres: Voltage::new::<microvolt>(statb.get(chip).data().vres().as_microvolts() as f32),
                    })
                },
            })
        }
    }

    impl CacheData {
        /// Updates StatusB cache with new data.
        ///
        /// This doesn't run the `autoconvert` function!! The caller should do that. Otherwise this update will basically do nothing
        ///
        /// ### Returns
        /// Will return `Ok(())`, or `Err(UpdateError)` if an error occurred. If this returns `Ok(())`, the cached data was updated correctly and can be read now.
        pub(in crate::segments) async fn update_status_b(&self, api: &mut alias::Api) -> Result<(), UpdateError> {
            self.statb.update(api).await?;

            Ok(())
        }

        /// Gets the current cached StatusB data.
        pub fn get_status_b(&self) -> status_b::Raw {
            status_b::Raw { statb: self.statb.data() }
        }
    }
}

/// Cache for PwmA and PwmB register groups.
pub mod pwm {
    use super::*;
    use super::alias;
    use adbms6830b::chip::registers::pwm::types::PwmDutyCycleConfig;
    use crate::segments::chips::cells::{IndexByCell, CellId};

    /// Raw PwmA/B register readings.
    pub struct Raw {
        pub pwma: RegisterCacheData<PwmA>,
        pub pwmb: RegisterCacheData<PwmB>,
    }
    impl Raw {
        /// Tries to make it nice.
        ///
        /// If no data has been read to that cache yet, this will return `Err(())`.
        pub fn try_nice(&self) -> Result<NiceData, ()> {
            NiceData::try_from(self)
        }
    }
    // ^^ note: this struct is just meant to be a nice helper for formatting returned data. the `CacheData` struct is still meant to directly hold these registers itself

    /// "Nice data" for a single chip.
    ///
    /// This leaves out cells 14 through 16 since we only have 13 cells.
    pub struct NiceDataChip {
        inner: IndexByCell<PwmDutyCycleConfig>,
    }
    impl core::ops::Deref for NiceDataChip {
        type Target = IndexByCell<PwmDutyCycleConfig>;

        fn deref(&self) -> &Self::Target {
            &self.inner
        }
    }

    /// Represents the raw register readings, but formatted in a more readable way.
    ///
    /// This doesn't contain any metadata about the reading (e.g., PEC errors). So you should
    /// probably inspect that stuff from the `Raw` readings before converting to this.
    pub struct NiceData {
        inner: IndexByChip<NiceDataChip>,
    }
    impl core::ops::Deref for NiceData {
        type Target = IndexByChip<NiceDataChip>;

        fn deref(&self) -> &Self::Target {
            &self.inner
        }
    }
    impl TryFrom<&Raw> for NiceData {
        type Error = ();

        /// Attempts to convert `Raw` data into a `NiceData`. If any of the registers involved
        /// in this `NiceData` haven't been read yet, this returns `Err(())`.
        fn try_from(raw: &Raw) -> Result<Self, Self::Error> {
            let Some(pwma) = raw.pwma.data() else {
                return Err(());
            };
            let Some(pwmb) = raw.pwmb.data() else {
                return Err(());
            };

            Ok(Self {
                inner: {
                    IndexByChip::from_fn(|chip| NiceDataChip {
                        inner: IndexByCell::from_fn(|cell| match cell {
                            CellId::Cell1 => pwma.get(chip).data().pwm1(),
                            CellId::Cell2 => pwma.get(chip).data().pwm2(),
                            CellId::Cell3 => pwma.get(chip).data().pwm3(),
                            CellId::Cell4 => pwma.get(chip).data().pwm4(),
                            CellId::Cell5 => pwma.get(chip).data().pwm5(),
                            CellId::Cell6 => pwma.get(chip).data().pwm6(),
                            CellId::Cell7 => pwma.get(chip).data().pwm7(),
                            CellId::Cell8 => pwma.get(chip).data().pwm8(),
                            CellId::Cell9 => pwma.get(chip).data().pwm9(),
                            CellId::Cell10 => pwma.get(chip).data().pwm10(),
                            CellId::Cell11 => pwma.get(chip).data().pwm11(),
                            CellId::Cell12 => pwma.get(chip).data().pwm12(),
                            CellId::Cell13 => pwmb.get(chip).data().pwm13(),
                        }),
                    })
                },
            })
        }
    }

    impl CacheData {
        /// Updates PwmA/B cache with new data.
        ///
        /// ### Returns
        /// Will return `Ok(())`, or `Err(UpdateError)` if an error occurred. If this returns `Ok(())`, the cached data was updated correctly and can be read now.
        pub(in crate::segments) async fn update_pwm(&self, api: &mut alias::Api) -> Result<(), UpdateError> {
            self.pwma.update(api).await?;
            self.pwmb.update(api).await?;

            Ok(())
        }

        /// Gets the current cached PwmA/B data.
        pub fn get_pwm(&self) -> pwm::Raw {
            pwm::Raw { pwma: self.pwma.data(), pwmb: self.pwmb.data() }
        }
    }
}
