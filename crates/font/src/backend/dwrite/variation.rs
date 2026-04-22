use anyhow::{Context, Result, anyhow};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_ITALIC, DWRITE_FONT_STYLE_NORMAL,
    DWRITE_FONT_WEIGHT_BOLD, DWRITE_FONT_WEIGHT_NORMAL, IDWriteFactory, IDWriteFactory6,
    IDWriteFontCollection, IDWriteFontFace2,
};
use windows::core::PCWSTR;
use windows_core::{BOOL, Interface};

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

/// Resolve one face per style using WT-style DirectWrite collection APIs.
///
/// Ghostty reference:
/// - Config/lifecycle-owned variation identity in `SharedGridSet.Key`
/// - Variation application during face load in `DeferredFace`
pub fn resolve_primary_faces(
    factory: &IDWriteFactory6,
    requests: &[StyleVariationRequest<'_>; Style::COUNT],
) -> Result<[IDWriteFontFace2; Style::COUNT]> {
    let first_collection = system_font_collection(factory, false)?;
    match resolve_primary_faces_in_collection(&first_collection, requests) {
        Ok(faces) => Ok(faces),
        Err(first_err) => {
            let refreshed_collection = system_font_collection(factory, true)?;
            resolve_primary_faces_in_collection(&refreshed_collection, requests).with_context(
                || format!("failed resolving primary faces after retry: {first_err:#}"),
            )
        }
    }
}

fn resolve_primary_faces_in_collection(
    collection: &IDWriteFontCollection,
    requests: &[StyleVariationRequest<'_>; Style::COUNT],
) -> Result<[IDWriteFontFace2; Style::COUNT]> {
    let mut out: [Option<IDWriteFontFace2>; Style::COUNT] = std::array::from_fn(|_| None);

    for style in Style::ALL {
        let req = &requests[style as usize];
        out[style as usize] =
            Some(resolve_style_face(collection, style, req).with_context(|| {
                format!("failed to resolve style={style:?} family='{}'", req.family)
            })?);
    }

    Ok(out.map(|v| v.expect("all styles resolved")))
}

fn system_font_collection(
    factory: &IDWriteFactory6,
    check_for_updates: bool,
) -> Result<IDWriteFontCollection> {
    let factory_base = factory.cast::<IDWriteFactory>()?;
    let mut out = None;
    unsafe { factory_base.GetSystemFontCollection(&mut out, check_for_updates) }?;
    out.ok_or_else(|| anyhow!("DirectWrite returned no system font collection"))
}

fn resolve_style_face(
    system_collection: &IDWriteFontCollection,
    style: Style,
    request: &StyleVariationRequest<'_>,
) -> Result<IDWriteFontFace2> {
    if request.family.trim().is_empty() {
        return Err(anyhow!("empty family name for style {style:?}"));
    }

    if let Ok(face) = resolve_style_face_for_family(system_collection, style, request.family) {
        return Ok(face);
    }

    if !request.family.eq_ignore_ascii_case("Consolas") {
        if let Ok(face) = resolve_style_face_for_family(system_collection, style, "Consolas") {
            return Ok(face);
        }
    }

    Err(anyhow!(
        "no usable face for style={style:?} after trying '{}'{}",
        request.family,
        if request.family.eq_ignore_ascii_case("Consolas") {
            ""
        } else {
            " and 'Consolas'"
        }
    ))
}

fn resolve_style_face_for_family(
    system_collection: &IDWriteFontCollection,
    style: Style,
    family_name: &str,
) -> Result<IDWriteFontFace2> {
    let mut family_utf16 = family_name.encode_utf16().collect::<Vec<u16>>();
    family_utf16.push(0);
    let mut index = 0;
    let mut exists = BOOL(0);
    let family_name_w = PCWSTR(family_utf16.as_ptr());
    unsafe { system_collection.FindFamilyName(family_name_w, &mut index, &mut exists) }?;
    if !exists.as_bool() {
        return Err(anyhow!(
            "no matching family for family='{family_name}' style={style:?}"
        ));
    }

    let family = unsafe { system_collection.GetFontFamily(index) }?;
    let weight = if style.is_bold() {
        DWRITE_FONT_WEIGHT_BOLD
    } else {
        DWRITE_FONT_WEIGHT_NORMAL
    };
    let font_style = if style.is_italic() {
        DWRITE_FONT_STYLE_ITALIC
    } else {
        DWRITE_FONT_STYLE_NORMAL
    };

    let font =
        unsafe { family.GetFirstMatchingFont(weight, DWRITE_FONT_STRETCH_NORMAL, font_style) }?;
    let face = unsafe { font.CreateFontFace() }?;
    Ok(face.cast::<IDWriteFontFace2>()?)
}
