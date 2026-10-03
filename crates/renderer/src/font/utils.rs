/// Returns Windows Terminal's polynomial coefficients for matching
/// DirectWrite's grayscale gamma correction in the shader.
///
/// WT reference: `src/renderer/atlas/dwrite_helpers.cpp:DWrite_GetGammaRatios`.
#[inline]
pub fn get_gamma_correction_ratios(gamma: f32) -> [f32; 4] {
    // WT's coefficient table for gamma values from 1.0 through 2.2.
    const GAMMA_CORRECTION_RATIOS: [[f32; 4]; 13] = [
        [0.0000 / 4.0, 0.0000 / 4.0, 0.0000 / 4.0, 0.0000 / 4.0], // gamma = 1.0
        [0.0166 / 4.0, -0.0807 / 4.0, 0.2227 / 4.0, -0.0751 / 4.0], // gamma = 1.1
        [0.0350 / 4.0, -0.1760 / 4.0, 0.4325 / 4.0, -0.1370 / 4.0], // gamma = 1.2
        [0.0543 / 4.0, -0.2821 / 4.0, 0.6302 / 4.0, -0.1876 / 4.0], // gamma = 1.3
        [0.0739 / 4.0, -0.3963 / 4.0, 0.8167 / 4.0, -0.2287 / 4.0], // gamma = 1.4
        [0.0933 / 4.0, -0.5161 / 4.0, 0.9926 / 4.0, -0.2616 / 4.0], // gamma = 1.5
        [0.1121 / 4.0, -0.6395 / 4.0, 1.1588 / 4.0, -0.2877 / 4.0], // gamma = 1.6
        [0.1300 / 4.0, -0.7649 / 4.0, 1.3159 / 4.0, -0.3080 / 4.0], // gamma = 1.7
        [0.1469 / 4.0, -0.8911 / 4.0, 1.4644 / 4.0, -0.3234 / 4.0], // gamma = 1.8
        [0.1627 / 4.0, -1.0170 / 4.0, 1.6051 / 4.0, -0.3347 / 4.0], // gamma = 1.9
        [0.1773 / 4.0, -1.1420 / 4.0, 1.7385 / 4.0, -0.3426 / 4.0], // gamma = 2.0
        [0.1908 / 4.0, -1.2652 / 4.0, 1.8650 / 4.0, -0.3476 / 4.0], // gamma = 2.1
        [0.2031 / 4.0, -1.3864 / 4.0, 1.9851 / 4.0, -0.3501 / 4.0], // gamma = 2.2
    ];

    // Convert WT's 256-based coefficients to the shader's 255-based UNORM domain.
    // WT's table entries are divided by four, so the trailing `* 4.0` cancels that scale.
    //
    // Coefficients x and z multiply foreground intensity and therefore use two
    // 256/255 scale factors; The offset coefficients y and w use only one.
    const NORM_XZ: f32 = ((0x10000 as f64) / (255.0 * 255.0) * 4.0) as f32;
    const NORM_YW: f32 = ((0x100 as f64) / (255.0) * 4.0) as f32;

    // Select the nearest supported gamma value.
    let index = ((gamma * 10.0 + 0.5) as isize).clamp(10, 22) as usize - 10;
    let ratios = GAMMA_CORRECTION_RATIOS[index];

    [ratios[0] * NORM_XZ, ratios[1] * NORM_YW, ratios[2] * NORM_XZ, ratios[3] * NORM_YW]
}
