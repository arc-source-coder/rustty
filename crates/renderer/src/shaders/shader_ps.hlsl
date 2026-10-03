#include "shader_common.hlsl"

cbuffer PsGlobals : register(b0) {
    float4 backgroundColor;
    float2 backgroundCellSizeInv;
    uint2 gridSize;
}

cbuffer CursorGlobals : register(b1) {
    float4 cursorRect;
    float4 cursorTextColor;
    float4 cursorTextCorrection;
}

StructuredBuffer<uint> backgroundCells : register(t0);
Texture2D<float4> grayscaleAtlas : register(t1);
Texture2D<float4> colorAtlas : register(t2);

float4 unpack_rgba(uint rgba) {
    return float4(uint4(rgba, rgba >> 8, rgba >> 16, rgba >> 24) & 0xffu) * (1.0 / 255.0);
}

float4 backgroundMain(PSData data) : SV_Target {
    uint2 cell = uint2(data.position.xy * backgroundCellSizeInv);

    if (all(cell < gridSize)) {
        uint index = cell.y * gridSize.x + cell.x;
        float4 overlay = unpack_rgba(backgroundCells[index]);
        return overlay + backgroundColor * (1.0 - overlay.a);
    }

    return backgroundColor;
}

float4 shadeGrayscale(PSData data, float4 color, float4 correction) {
    float alpha = grayscaleAtlas[data.texcoord].a;

    // DirectWrite enhanced contrast: c = alpha(k + 1) / (alpha * k + 1)
    float coverage = mad(alpha, correction.x, alpha)
        / mad(alpha, correction.x, 1.0);

    // Evaluate DirectWrite's color-dependent gamma correction in Horner form:
    //   c + c(1 - c)(pc + q)  =>  c((-pc + (p - q))c + (1 + q))
    //
    // correction.yzw contains (-p, p - q, 1 + q).
    float corrected = coverage * mad(
        mad(correction.y, coverage, correction.z),
        coverage,
        correction.w
    );

    return corrected * color;
}

float4 foregroundMain(PSData data) : SV_Target {
    switch (data.shadingType) {
        case SHADING_TYPE_GRAYSCALE_TEXT: {
            return shadeGrayscale(data, data.color, data.textCorrection);
        }
        case SHADING_TYPE_CURSOR_CANDIDATE: {
            bool underCursor = all(
                (data.position.xy >= cursorRect.xy) &&
                (data.position.xy < cursorRect.zw)
            );
            float4 color = underCursor ? cursorTextColor : data.color;
            float4 correction = underCursor ? cursorTextCorrection : data.textCorrection;
            return shadeGrayscale(data, color, correction);
        }
        case SHADING_TYPE_COLOR_TEXT: {
            return colorAtlas[data.texcoord];
        }
        default: {
            // Cursor/decorations/selection (solid premultiplied RGBA).
            return data.color;
        }
    }
}
