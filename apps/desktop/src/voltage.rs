/// Converts reconstructed signed ADS8691 ADC16 values to display volts for
/// the selected +/-5.12 V range. Backend events and scientific files retain
/// their original ADC/code10 values; only desktop cache ingress calls this.
#[inline]
#[must_use]
pub fn adc16_to_volts(adc: f64) -> f64 {
    adc * 0.000_156_25
}

#[cfg(test)]
mod tests {
    use super::adc16_to_volts;

    #[test]
    fn hardware_voltage_cases_match_literal_values_and_two_decimal_readings() {
        for (code, adc, volts, displayed) in [
            (0, -32_768, -5.12, "-5.12"),
            (256, -16_384, -2.56, "-2.56"),
            (512, 0, 0.00, "0.00"),
            (701, 12_096, 1.89, "1.89"),
            (702, 12_160, 1.90, "1.90"),
            (825, 20_032, 3.13, "3.13"),
            (1023, 32_704, 5.11, "5.11"),
        ] {
            assert_eq!(langmuir_core::reconstruct_adc16(code), adc);
            let actual = adc16_to_volts(f64::from(adc));
            assert!((actual - volts).abs() < 1.0e-12);
            assert!((actual - (f64::from(code) * 0.01 - 5.12)).abs() < 1.0e-12);
            assert_eq!(format!("{actual:.2}"), displayed);
        }
    }
}
