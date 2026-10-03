#define SHADING_TYPE_BACKGROUND 0
#define SHADING_TYPE_SOLID 1
#define SHADING_TYPE_GRAYSCALE_TEXT 2
#define SHADING_TYPE_COLOR_TEXT 3
#define SHADING_TYPE_CURSOR_CANDIDATE 4

#define SIZE_MASK 0x7fff

struct VSData {
    int2 position : position;
    // Contains shadingType packed into the high bits.
    // Size is packed into the lower 15 bits.
    uint2 packed_data : packed_data;
    uint2 texcoord : texcoord;
    float4 color : color;
};

struct PSData {
    float4 position : SV_Position;
    float2 texcoord : texcoord;

    nointerpolation uint shadingType : shadingType;
    nointerpolation float4 color : color;

    // DirectWrite grayscale correction coefficients:
    //   x = enhanced-contrast coefficient k.
    //   yzw = (-p, p - q, 1 + q), the Horner coefficients for c + c(1 - c)(pc + q).
    nointerpolation float4 textCorrection : textCorrection;
};
