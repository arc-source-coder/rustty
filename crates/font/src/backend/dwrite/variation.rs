use anyhow::{Result, anyhow};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FONT_AXIS_TAG_WEIGHT, DWRITE_FONT_AXIS_VALUE, DWRITE_FONT_PROPERTY,
    DWRITE_FONT_PROPERTY_ID_FAMILY_NAME, DWRITE_FONT_SIMULATIONS_NONE, IDWriteFactory6,
    IDWriteFontFace2, IDWriteFontResource, IDWriteFontSet1,
};
use windows::core::PCWSTR;
use windows_core::Interface;

use crate::types::{FontAxisSpec, Style};

/// Config-time request for one styled primary face.
///
/// Ghostty reference:
/// `font.SharedGridSet.Key` + `font.discovery.Descriptor` family/style/variation
/// descriptors are resolved before runtime rendering.
#[derive(Clone, Debug)]
pub struct StyleVariationRequest<'a> {
    pub family: &'a str,
    pub axes: FontAxisSpec,
}

/// Resolve one face per style using DirectWrite font-set APIs available on
/// Windows 10 build 16299 (`dwrite_3.h`).
///
/// This intentionally avoids runtime `MapCharacters` axis mutation and binds
/// variation coordinates at face-construction time.
///
/// Ghostty reference:
/// - Config/lifecycle-owned variation identity in `SharedGridSet.Key`
/// - Variation application during face load in `DeferredFace`
pub fn resolve_primary_faces(
    factory: &IDWriteFactory6,
    requests: &[StyleVariationRequest<'_>; Style::COUNT],
) -> Result<[IDWriteFontFace2; Style::COUNT]> {
    let system_set = unsafe { factory.GetSystemFontSet(false) }?;
    let mut out: [Option<IDWriteFontFace2>; Style::COUNT] = std::array::from_fn(|_| None);

    for style in Style::ALL {
        let req = &requests[style as usize];
        out[style as usize] = Some(resolve_style_face(&system_set, style, req)?);
    }

    Ok(out.map(|v| v.expect("all styles resolved")))
}

fn resolve_style_face(
    system_set: &IDWriteFontSet1,
    style: Style,
    request: &StyleVariationRequest<'_>,
) -> Result<IDWriteFontFace2> {
    if request.family.trim().is_empty() {
        return Err(anyhow!("empty family name for style {style:?}"));
    }

    let mut family_utf16 = request.family.encode_utf16().collect::<Vec<u16>>();
    family_utf16.push(0);
    let property = DWRITE_FONT_PROPERTY {
        propertyId: DWRITE_FONT_PROPERTY_ID_FAMILY_NAME,
        propertyValue: PCWSTR(family_utf16.as_ptr()),
        localeName: PCWSTR::null(),
    };

    // Start matching from explicitly configured axes only; style defaults are
    // applied later against the selected font resource defaults.
    let matched_axes = request.axes.values.clone();

    let matched = unsafe {
        system_set.GetMatchingFonts(
            Some(&property as *const DWRITE_FONT_PROPERTY),
            &matched_axes,
        )
    }?;
    let count = unsafe { matched.GetFontCount() };
    if count == 0 {
        return Err(anyhow!(
            "no matching face for family='{}' style={style:?}",
            request.family
        ));
    }

    // Create a font resource for the selected instance, then derive defaults
    // from that resource and apply style/user overrides before creating the
    // final face.
    //
    // Ghostty reference:
    // style/variation config is applied at face-load time in DeferredFace.
    //
    // Windows 10 RS3-compatible path:
    // we intentionally avoid IDWriteFontSet4::ConvertWeightStretchStyleToFontAxisValues
    // (newer API) and instead build from IDWriteFontResource defaults.
    // Ghostty parity: choose the first/best match from discovery results rather
    // than scanning all candidates with additional heuristics.
    let resource = unsafe { matched.CreateFontResource(0) }?;
    let mut resolved_axes = font_resource_default_axes(&resource)?;
    let default_weight = resolved_axes
        .iter()
        .find(|v| v.axisTag == DWRITE_FONT_AXIS_TAG_WEIGHT)
        .map(|v| v.value.max(0.0).round() as u16)
        .unwrap_or(400);
    request
        .axes
        .resolve_with_variant_defaults_into(style, default_weight, &mut resolved_axes);
    let face5 = unsafe { resource.CreateFontFace(DWRITE_FONT_SIMULATIONS_NONE, &resolved_axes) }?;
    Ok(face5.cast::<IDWriteFontFace2>()?)
}

fn font_resource_default_axes(
    resource: &IDWriteFontResource,
) -> Result<Vec<DWRITE_FONT_AXIS_VALUE>> {
    let axis_count = unsafe { resource.GetFontAxisCount() } as usize;
    let mut axes = vec![DWRITE_FONT_AXIS_VALUE::default(); axis_count];
    if axis_count > 0 {
        unsafe { resource.GetDefaultFontAxisValues(&mut axes) }?;
    }
    Ok(axes)
}
