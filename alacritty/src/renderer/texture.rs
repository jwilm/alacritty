use crate::gl;
use crate::gl::types::*;

use std::ptr;

pub enum PixelFormat {
    RGBA8,
    RGB8,
}

pub struct TextureFormat {
    internal: i32,
    format: u32,
    texel_type: u32,
}

pub fn get_gl_format(format: PixelFormat) -> TextureFormat {
    match format {
        PixelFormat::RGBA8 => TextureFormat {
            internal: gl::RGBA as i32,
            format: gl::RGBA,
            texel_type: gl::UNSIGNED_BYTE,
        },
        PixelFormat::RGB8 => TextureFormat {
            internal: gl::RGB as i32,
            format: gl::RGB,
            texel_type: gl::UNSIGNED_BYTE,
        },
    }
}

pub unsafe fn upload_texture(
    width: i32,
    height: i32,
    format: PixelFormat,
    ptr: *const libc::c_void,
) {
    let format = get_gl_format(format);
    gl::TexImage2D(
        gl::TEXTURE_2D,
        0,
        format.internal,
        width,
        height,
        0,
        format.format,
        format.texel_type,
        ptr,
    );
}

pub unsafe fn create_texture(width: i32, height: i32, format: PixelFormat) -> GLuint {
    let mut id: GLuint = 0;
    let format = get_gl_format(format);

    gl::PixelStorei(gl::UNPACK_ALIGNMENT, 1);

    gl::GenTextures(1, &mut id);
    gl::BindTexture(gl::TEXTURE_2D, id);
    gl::TexImage2D(
        gl::TEXTURE_2D,
        0,
        format.internal,
        width,
        height,
        0,
        format.format,
        format.texel_type,
        ptr::null(),
    );

    gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_WRAP_S, gl::CLAMP_TO_EDGE as i32);
    gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_WRAP_T, gl::CLAMP_TO_EDGE as i32);
    gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_MIN_FILTER, gl::NEAREST as i32);
    gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_MAG_FILTER, gl::NEAREST as i32);
    // gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_MAG_FILTER, gl::LINEAR as i32);

    gl::BindTexture(gl::TEXTURE_2D, 0);
    id
}
