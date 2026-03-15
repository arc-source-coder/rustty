// Unified WT-style quad shader path.
#define SHADING_TYPE_BACKGROUND 0
#define SHADING_TYPE_TEXT_GRAYSCALE 1
#define SHADING_TYPE_TEXT_COLOR 2
#define SHADING_TYPE_SOLID_LINE 8

struct VSData {
    float2 vertex : SV_Position;
    uint shadingType : shadingType;
    uint2 renditionScale : renditionScale;
    int2 position : position;
    uint2 size : size;
    uint2 texcoord : texcoord;
    float4 color : color;
};

struct PSData {
    float4 position : SV_Position;
    float2 texcoord : texcoord;
    nointerpolation uint shadingType : shadingType;
    nointerpolation float2 renditionScale : renditionScale;
    nointerpolation float4 color : color;
};

cbuffer Globals : register(b0) {
    float2 positionScale;
    float2 _pad0;
    float4 backgroundColor;
    float2 backgroundCellSize;
    float2 backgroundCellCount;
}

Texture2D<float4> t_background_cells : register(t0);
Texture2D<float> t_atlas_grayscale : register(t1);
Texture2D<float4> t_atlas_color : register(t2);
SamplerState s_atlas : register(s0);

PSData renderer_vertex(VSData data)
{
    PSData output;
    output.color = data.color;
    output.shadingType = data.shadingType;
    output.renditionScale = data.renditionScale;
    output.position.xy = (data.position + data.vertex * data.size) * positionScale + float2(-1.0f, 1.0f);
    output.position.zw = float2(0.0f, 1.0f);
    output.texcoord = data.texcoord + data.vertex * data.size;
    return output;
}

float4 renderer_fragment(PSData data) : SV_Target
{
    if (data.shadingType == SHADING_TYPE_BACKGROUND) {
        int2 cell = int2(floor(data.position.xy / backgroundCellSize));
        if (cell.x < 0 || cell.y < 0 || cell.x >= (int)backgroundCellCount.x || cell.y >= (int)backgroundCellCount.y) {
            return backgroundColor;
        }

        float4 color = t_background_cells.Load(int3(cell, 0));
        color.a = 1.0f;
        return color;
    }

    if (data.shadingType == SHADING_TYPE_TEXT_GRAYSCALE) {
        float coverage = t_atlas_grayscale.Load(int3(int2(data.texcoord), 0)).r;
        float alpha = coverage * data.color.a;
        return float4(data.color.rgb * alpha, alpha);
    }

    if (data.shadingType == SHADING_TYPE_TEXT_COLOR) {
        return t_atlas_color.Load(int3(int2(data.texcoord), 0));
    }

    // Cursor/decorations/selection (solid premultiplied RGBA).
    return float4(data.color.rgb * data.color.a, data.color.a);
}
