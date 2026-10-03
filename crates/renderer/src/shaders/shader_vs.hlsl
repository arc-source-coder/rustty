#include "shader_common.hlsl"

cbuffer VsGlobals : register(b0) {
    float2 positionScale;
    float2 _positionPadding;

    float4 gammaRatios;
    float grayscaleEnhancedContrast;
    float3 _textPadding;
}

cbuffer CursorGlobals : register(b1) {
    float4 cursorRect;
    float4 cursorTextColor;
    float4 cursorTextCorrection;
}

PSData main(uint vertexId : SV_VertexID, VSData data) {
    PSData output;

    // Shading type is packed into bit 15 of size.x and bit 15 of size.y.
    uint shadingType = (data.packed_data.x >> 15) | ((data.packed_data.y >> 15) << 1);
    output.shadingType = shadingType;

    output.color = float4(data.color.rgb * data.color.a, data.color.a);

    // DirectWrite's grayscale gamma correction depends on foreground intensity.
    // These values are constant across the glyph quad, so calculate them here
    // instead of repeating the work for every glyph pixel.
    //
    // WT reference:
    //  src/renderer/atlas/dwrite_helpers.hlsl
    //  DWrite_CalcColorIntensity / DWrite_ApplyAlphaCorrection
    float intensity = dot(data.color.rgb, float3(0.25, 0.50, 0.25));

    float p = gammaRatios.x * intensity + gammaRatios.y;
    float q = gammaRatios.z * intensity + gammaRatios.w;

    float contrast = grayscaleEnhancedContrast *
        saturate(dot(data.color.rgb, float3(0.30f, 0.59f, 0.11f) * -4.0f) + 3.0);

    // WT's text correction, c + c(1 - c)(pc + q), written in Horner form.
    //
    // Packing these coefficients here leaves only coverage-dependent work in
    // the pixel shader. The x component stores the separate contrast coefficient.
    output.textCorrection = float4(
        contrast,
        -p,
        p - q,
        1.0 + q
    );

    // Clear the top bits to get the actual dimensions.
    uint2 size = data.packed_data & SIZE_MASK;
    if (shadingType == SHADING_TYPE_GRAYSCALE_TEXT) {
        float2 quadMin = float2(data.position);
        float2 quadMax = quadMin + float2(size);
        // Only glyph quads intersecting the cursor need the exact pixel test.
        if (all((quadMax > cursorRect.xy) && (quadMin < cursorRect.zw))) {
            output.shadingType = SHADING_TYPE_CURSOR_CANDIDATE;
        }
    }

    uint2 corner = uint2(vertexId & 1, vertexId >> 1);
    uint2 vertex_offset = corner * size;

    output.position.xy = (data.position + vertex_offset) * positionScale + float2(-1.0f, 1.0f);
    output.position.zw = float2(0.0f, 1.0f);
    output.texcoord = data.texcoord + vertex_offset;

    return output;
}
