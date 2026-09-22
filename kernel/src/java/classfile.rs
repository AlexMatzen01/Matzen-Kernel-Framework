//! Copyright (c) Alexander Matzen. All rights reserved.
//! Author: Alexander Matzen
//! Licensed under the MIT license.

//! Bounds-checked Java class-file parser.
//!
//! This intentionally keeps only the metadata needed by the Phase 2
//! interpreter. Unknown attributes are skipped, so adding debug attributes or
//! compiler metadata does not make an otherwise valid class unloadable.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use super::class::{parse_header, ClassInfo};

#[derive(Clone, Debug)]
pub enum Constant {
    Unusable,
    Utf8(String),
    Integer(i32),
    Float(u32),
    Long(i64),
    Double(u64),
    Class(u16),
    String(u16),
    Fieldref { class: u16, name_type: u16 },
    Methodref { class: u16, name_type: u16 },
    InterfaceMethodref { class: u16, name_type: u16 },
    NameAndType { name: u16, descriptor: u16 },
}

#[derive(Clone, Debug)]
pub struct ExceptionHandler {
    pub start_pc: u16,
    pub end_pc: u16,
    pub handler_pc: u16,
    pub catch_type: u16,
}

#[derive(Clone, Debug)]
pub struct MethodInfo {
    pub access_flags: u16,
    pub name: String,
    pub descriptor: String,
    pub max_stack: u16,
    pub max_locals: u16,
    pub code: Vec<u8>,
    pub exceptions: Vec<ExceptionHandler>,
}

#[derive(Clone, Debug)]
pub struct ClassFile {
    pub version: ClassInfo,
    pub constants: Vec<Constant>,
    pub access_flags: u16,
    pub this_class: u16,
    pub super_class: u16,
    pub methods: Vec<MethodInfo>,
}

impl ClassFile {
    pub fn class_name(&self, index: u16) -> Result<&str, &'static str> {
        let name_index = match self.constant(index)? {
            Constant::Class(index) => *index,
            _ => return Err("Expected class constant"),
        };
        self.utf8(name_index)
    }

    pub fn utf8(&self, index: u16) -> Result<&str, &'static str> {
        match self.constant(index)? {
            Constant::Utf8(value) => Ok(value.as_str()),
            _ => Err("Expected UTF-8 constant"),
        }
    }

    pub fn method_ref(&self, index: u16) -> Result<(&str, &str, &str), &'static str> {
        let (class, name_type) = match self.constant(index)? {
            Constant::Methodref { class, name_type }
            | Constant::InterfaceMethodref { class, name_type } => (*class, *name_type),
            _ => return Err("Expected method reference"),
        };
        let (name, descriptor) = match self.constant(name_type)? {
            Constant::NameAndType { name, descriptor } => (*name, *descriptor),
            _ => return Err("Expected name/type constant"),
        };
        Ok((
            self.class_name(class)?,
            self.utf8(name)?,
            self.utf8(descriptor)?,
        ))
    }

    pub fn field_ref(&self, index: u16) -> Result<(&str, &str, &str), &'static str> {
        let (class, name_type) = match self.constant(index)? {
            Constant::Fieldref { class, name_type } => (*class, *name_type),
            _ => return Err("Expected field reference"),
        };
        let (name, descriptor) = match self.constant(name_type)? {
            Constant::NameAndType { name, descriptor } => (*name, *descriptor),
            _ => return Err("Expected name/type constant"),
        };
        Ok((
            self.class_name(class)?,
            self.utf8(name)?,
            self.utf8(descriptor)?,
        ))
    }

    pub fn string_constant(&self, index: u16) -> Result<&str, &'static str> {
        let value = match self.constant(index)? {
            Constant::String(value) => *value,
            _ => return Err("Expected string constant"),
        };
        self.utf8(value)
    }

    pub fn integer_constant(&self, index: u16) -> Result<i32, &'static str> {
        match self.constant(index)? {
            Constant::Integer(value) => Ok(*value),
            _ => Err("Expected integer constant"),
        }
    }

    pub fn find_method(&self, name: &str, descriptor: &str) -> Option<&MethodInfo> {
        self.methods
            .iter()
            .find(|m| m.name == name && m.descriptor == descriptor)
    }

    fn constant(&self, index: u16) -> Result<&Constant, &'static str> {
        self.constants
            .get(index as usize)
            .ok_or("Constant-pool index out of bounds")
    }
}

pub fn parse(data: &[u8]) -> Result<ClassFile, &'static str> {
    let version = parse_header(data)?;
    let mut reader = Reader::new(&data[8..]);
    let constant_count = reader.u16()? as usize;
    if constant_count < 1 || constant_count > 16_384 {
        return Err("Invalid constant-pool count");
    }
    let mut constants = Vec::with_capacity(constant_count);
    constants.push(Constant::Unusable);
    let mut index = 1;
    while index < constant_count {
        let constant = match reader.u8()? {
            1 => Constant::Utf8(reader.string()?),
            3 => Constant::Integer(reader.i32()?),
            4 => Constant::Float(reader.u32()?),
            5 => {
                let value = ((reader.u32()? as i64) << 32) | reader.u32()? as i64 & 0xffff_ffff;
                constants.push(Constant::Long(value));
                constants.push(Constant::Unusable);
                index += 2;
                continue;
            }
            6 => {
                let value = ((reader.u32()? as u64) << 32) | reader.u32()? as u64;
                constants.push(Constant::Double(value));
                constants.push(Constant::Unusable);
                index += 2;
                continue;
            }
            7 => Constant::Class(reader.u16()?),
            8 => Constant::String(reader.u16()?),
            9 => Constant::Fieldref {
                class: reader.u16()?,
                name_type: reader.u16()?,
            },
            10 => Constant::Methodref {
                class: reader.u16()?,
                name_type: reader.u16()?,
            },
            11 => Constant::InterfaceMethodref {
                class: reader.u16()?,
                name_type: reader.u16()?,
            },
            12 => Constant::NameAndType {
                name: reader.u16()?,
                descriptor: reader.u16()?,
            },
            tag => {
                return Err(if tag == 15 || tag == 16 || tag == 18 {
                    "Unsupported Java 7+ constant-pool tag"
                } else {
                    "Unknown constant-pool tag"
                })
            }
        };
        constants.push(constant);
        index += 1;
    }

    let access_flags = reader.u16()?;
    let this_class = reader.u16()?;
    let super_class = reader.u16()?;
    skip_interfaces(&mut reader)?;
    skip_members(&mut reader, &constants)?;
    let methods = read_methods(&mut reader, &constants)?;
    skip_attributes(&mut reader)?;

    Ok(ClassFile {
        version,
        constants,
        access_flags,
        this_class,
        super_class,
        methods,
    })
}

fn skip_interfaces(reader: &mut Reader<'_>) -> Result<(), &'static str> {
    for _ in 0..reader.u16()? {
        reader.u16()?;
    }
    Ok(())
}

fn skip_members(reader: &mut Reader<'_>, constants: &[Constant]) -> Result<(), &'static str> {
    for _ in 0..reader.u16()? {
        reader.u16()?;
        reader.u16()?;
        reader.u16()?;
        skip_attributes(reader)?;
    }
    let _ = constants;
    Ok(())
}

fn read_methods(
    reader: &mut Reader<'_>,
    constants: &[Constant],
) -> Result<Vec<MethodInfo>, &'static str> {
    let count = reader.u16()? as usize;
    if count > 4096 {
        return Err("Too many methods");
    }
    let mut methods = Vec::with_capacity(count);
    for _ in 0..count {
        let access_flags = reader.u16()?;
        let name = utf8(constants, reader.u16()?)?.to_string();
        let descriptor = utf8(constants, reader.u16()?)?.to_string();
        let attributes = reader.u16()? as usize;
        let mut max_stack = 0;
        let mut max_locals = 0;
        let mut code = Vec::new();
        let mut exceptions = Vec::new();
        for _ in 0..attributes {
            let attribute_name = utf8(constants, reader.u16()?)?;
            let length = reader.u32()? as usize;
            let bytes = reader.bytes(length)?;
            if attribute_name == "Code" {
                let mut code_reader = Reader::new(bytes);
                max_stack = code_reader.u16()?;
                max_locals = code_reader.u16()?;
                let code_len = code_reader.u32()? as usize;
                code = code_reader.bytes(code_len)?.to_vec();
                let exception_count = code_reader.u16()?;
                for _ in 0..exception_count {
                    exceptions.push(ExceptionHandler {
                        start_pc: code_reader.u16()?,
                        end_pc: code_reader.u16()?,
                        handler_pc: code_reader.u16()?,
                        catch_type: code_reader.u16()?,
                    });
                }
                // Nested Code attributes follow the exception table in the class format.
                skip_attributes(&mut code_reader)?;
            }
        }
        methods.push(MethodInfo {
            access_flags,
            name,
            descriptor,
            max_stack,
            max_locals,
            code,
            exceptions,
        });
    }
    Ok(methods)
}

fn skip_attributes(reader: &mut Reader<'_>) -> Result<(), &'static str> {
    for _ in 0..reader.u16()? {
        reader.u16()?;
        let length = reader.u32()? as usize;
        reader.bytes(length)?;
    }
    Ok(())
}

fn utf8(constants: &[Constant], index: u16) -> Result<&str, &'static str> {
    match constants.get(index as usize) {
        Some(Constant::Utf8(value)) => Ok(value.as_str()),
        _ => Err("Expected UTF-8 constant"),
    }
}

struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }
    fn bytes(&mut self, len: usize) -> Result<&'a [u8], &'static str> {
        let end = self
            .pos
            .checked_add(len)
            .ok_or("Class-file length overflow")?;
        if end > self.data.len() {
            return Err("Truncated class-file attribute");
        }
        let result = &self.data[self.pos..end];
        self.pos = end;
        Ok(result)
    }
    fn u8(&mut self) -> Result<u8, &'static str> {
        Ok(*self.bytes(1)?.first().unwrap())
    }
    fn u16(&mut self) -> Result<u16, &'static str> {
        let b = self.bytes(2)?;
        Ok(u16::from_be_bytes([b[0], b[1]]))
    }
    fn u32(&mut self) -> Result<u32, &'static str> {
        let b = self.bytes(4)?;
        Ok(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }
    fn i32(&mut self) -> Result<i32, &'static str> {
        Ok(self.u32()? as i32)
    }
    fn string(&mut self) -> Result<String, &'static str> {
        let length = self.u16()? as usize;
        let bytes = self.bytes(length)?;
        core::str::from_utf8(bytes)
            .map(String::from)
            .map_err(|_| "Invalid modified UTF-8")
    }
}
