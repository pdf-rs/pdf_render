 @vertex
fn vs_main(@builtin(vertex_index) ix: u32) -> @builtin(position) vec4<f32> {
    // Generate a full screen quad in normalized device coordinates
    var vertex = vec2(-1.0, 1.0);  // Top-left corner
    switch ix {
        case 1u: {
            vertex = vec2(-1.0, -1.0);  // Bottom-left corner
        }
        case 2u, 4u: {
            vertex = vec2(1.0, -1.0); // Bottom-right corner
        }
        case 5u: {
            vertex = vec2(1.0, 1.0); //Top-right corner
        }
        default: {}
    }
    return vec4(vertex, 0.0, 1.0);
}

@group(0) @binding(0)
var fine_output: texture_2d<f32>;

@fragment
fn fs_main(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    return textureLoad(fine_output, vec2<i32>(pos.xy), 0);
}
