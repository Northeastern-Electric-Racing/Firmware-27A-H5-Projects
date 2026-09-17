//! Debug data for segments.

use crate::segments;

/// Task that sends out debug segments data.
///
/// This is probably (?) just going to be a temporary thing until we get more organized (i.e., we may remove this task or rework it as the overall structure of shepherd-3 starts coming together).
/// For now, this is just meant to test out the segments subsystem and give an example of reading data from it.
#[embassy_executor::task]
pub async fn segments_debug() {
    use segments::{SEGMENTS_FRESH_DATA_SIGNAL, ChipId, ChipKind, CellId};
    use crate::units::{degree_celsius, volt};
    use crate::can;

    // Subscribe to Segments fresh data signal subscription so we are notified when new segments data comes in.
    let mut subscription = SEGMENTS_FRESH_DATA_SIGNAL.subscribe().expect("There are too many waiters on this signal. We should probably increase the waiters capacity.");

    loop {
        // Run one loop of this task every time new Segments data arrives.
        subscription.wait().await;

        // Get "raw" cache readings.
        let redundant_aux_raw = segments::cache().get_redundant_aux();
        let filtered_cell_voltages_raw = segments::cache().get_filtered_cell_voltages();
        let cell_voltages_raw = segments::cache().get_cell_voltages();
        let pwm_raw = segments::cache().get_pwm();
        let status_c_raw = segments::cache().get_status_c();
        let s_voltages_raw = segments::cache().get_s_voltages();
        let _fault_counts = segments::cache().get_fault_counts();

        // u_TODO - should probably inspect the PEC status and other metadata before transforming into NiceData, but i don't think TSECU-Shepherd does that so for now this is probably fine

        // Convert "raw" readings to NiceData. When `try_nice()` fails, it means that the cache hasn't been updated yet (since starting up), so we skip for now and go back to top of the loop.
        let Ok(redundant_aux) = redundant_aux_raw.try_nice() else {
            continue;
        };
        let Ok(filtered_cell_voltages) = filtered_cell_voltages_raw.try_nice() else {
            continue;
        };
        let Ok(_cell_voltages) = cell_voltages_raw.try_nice() else {
            continue;
        };
        let Ok(pwm) = pwm_raw.try_nice() else {
            continue;
        };
        let Ok(status_c) = status_c_raw.try_nice() else {
            continue;
        };
        let Ok(_s_voltages) = s_voltages_raw.try_nice() else {
            continue;
        };

        // Iterate through every chip and send data over CAN.
        #[cfg(true)]
        '_can: {
            for chip in ChipId::iter() {
                let temps = redundant_aux.chip(chip).to_temps().cell_temperatures;
                let volts = filtered_cell_voltages.chip(chip); // u_TODO - if charging, we use cell_voltages, if not charging, we use filtered_cell_voltages. This is what TSECU-Shepherd did. but there's no charging state rn so for now who cares
                let pwm = pwm.chip(chip);
                let cvs = status_c.chip(chip).cell_channel_comparison_faults;

                match chip.kind() {
                    ChipKind::Alpha => {
                        for (cell_a, cell_b) in CellId::iter_pairs() {
                            match can::try_send(
                                can::types::AlphaCellDataDebug {
                                    therm: temps.cell(cell_a).get::<degree_celsius>(),
                                    chip_id: chip.segment().as_u8(),

                                    // Cell A data.
                                    voltage_a: volts.cell(cell_a).get::<volt>(),
                                    cell_a: cell_a.as_u8(),
                                    discharging_a: pwm.cell(cell_a).is_balancing(),
                                    cvs_a: cvs.cell(cell_a).is_set(),
                                    ow_a: false,

                                    // Cell B data. When cell_b is `None` (due to the enum having an odd number of variants), just pass in random obviously-wrong numbers
                                    voltage_b: cell_b.map(|cell_b| volts.cell(cell_b).get::<volt>()).unwrap_or(14_f32),
                                    cell_b: cell_b.map(|cell_b| cell_b.as_u8()).unwrap_or(14),
                                    discharging_b: cell_b.map(|cell_b| pwm.cell(cell_b).is_balancing()).unwrap_or(false),
                                    cvs_b: cell_b.map(|cell_b| pwm.cell(cell_b).is_balancing()).unwrap_or(false),
                                    ow_b: false,
                                }
                                .as_frame(),
                            ) {
                                Ok(_) => (),
                                Err(_) => (),
                            }
                        }
                    },

                    ChipKind::Beta => {
                        for (cell_a, cell_b) in CellId::iter_pairs() {
                            match can::try_send(
                                can::types::BetaCellDataDebug {
                                    therm: temps.cell(cell_a).get::<degree_celsius>(),
                                    chip_id: chip.segment().as_u8(),

                                    // Cell A data.
                                    voltage_a: volts.cell(cell_a).get::<volt>(),
                                    cell_a: cell_a.as_u8(),
                                    discharging_a: pwm.cell(cell_a).is_balancing(),
                                    cvs_a: cvs.cell(cell_a).is_set(),
                                    ow_a: false,

                                    // Cell B data. When cell_b is `None` (due to the enum having an odd number of variants), just pass in random obviously-wrong numbers
                                    voltage_b: cell_b.map(|cell_b| volts.cell(cell_b).get::<volt>()).unwrap_or(14_f32),
                                    cell_b: cell_b.map(|cell_b| cell_b.as_u8()).unwrap_or(14),
                                    discharging_b: cell_b.map(|cell_b| pwm.cell(cell_b).is_balancing()).unwrap_or(false),
                                    cvs_b: cell_b.map(|cell_b| pwm.cell(cell_b).is_balancing()).unwrap_or(false),
                                    ow_b: false,
                                }
                                .as_frame(),
                            ) {
                                Ok(_) => (),
                                Err(_) => (),
                            }
                        }
                    },
                }
            }
        }

        #[cfg(defmt_monitor)]
        '_defmt_monitor: {
            for chip in ChipId::iter() {
                // Chip-level logs.
                defmt_monitor::monitor!(["SegmentDebug/Chips/Chip{=u8}/Segment", chip.as_u8()], desc = "What segment this chip is on (0 through 4).", "{=u8}", chip.segment().as_u8());
                defmt_monitor::monitor!(["SegmentDebug/Chips/Chip{=u8}/Kind", chip.as_u8()], desc = "If this chip is Alpha or Beta.", "{}", chip.kind());

                let volts = filtered_cell_voltages.chip(chip);
                let temps = redundant_aux.chip(chip).to_temps().cell_temperatures;
                let comparison_fault_counts = _fault_counts.chip(chip).csxflt.idx_by_cell();

                for cell in CellId::iter() {
                    // Cell-level logs.
                    defmt_monitor::monitor!(["SegmentDebug/Chips/Chip{=u8}/Cell{=u8}/Voltage", chip.as_u8(), cell.as_u8()], desc = "Cell voltage, in volts.", "{=f32}", volts.cell(cell).get::<volt>());
                    defmt_monitor::monitor!(["SegmentDebug/Chips/Chip{=u8}/Cell{=u8}/Temperature", chip.as_u8(), cell.as_u8()], desc = "Cell temperautre, in celsius.", "{=f32}", temps.cell(cell).get::<degree_celsius>());
                    defmt_monitor::monitor!(["SegmentDebug/Chips/Chip{=u8}/Cell{=u8}/ComparisonFaultCounts", chip.as_u8(), cell.as_u8()], desc = "Total number of comparison faults that have been read back for this cell so far since boot.", "{=u32}", comparison_fault_counts.cell(cell));
                }
            }
        }
    }
}
