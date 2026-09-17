//! Type aliases for units from the `uom` crate used by this project.

pub use uom::si::thermodynamic_temperature::degree_celsius;
pub use uom::si::electric_potential::volt;

/// Voltage!
///
/// Technically this is Electric Potential but who even calls it that
pub type Voltage = uom::si::f32::ElectricPotential;

/// Temperature!
pub type Temperature = uom::si::f32::ThermodynamicTemperature;

// Custom "microcelcius" unit (not really a real unit but adbms6830b returns temp scaled this way). Can be used with `Temperature`
uom::unit! {
    system: uom::si;
    quantity: uom::si::thermodynamic_temperature;

    @microcelcius: 1.0e-6, 273.15; "uC", "degree (microcelcius)", "degrees (microcelcius)";
}

/// Ohms and such
pub type Resistance = uom::si::f32::ElectricalResistance;
