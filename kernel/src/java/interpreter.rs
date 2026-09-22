//! Copyright (c) Alexander Matzen. All rights reserved.
//! Author: Alexander Matzen
//! Licensed under the MIT license.

//! Small, cooperative Java bytecode interpreter.

use alloc::string::String;
use alloc::vec::Vec;

use super::classfile::{ClassFile, Constant, MethodInfo};

const MAX_STEPS: usize = 100_000;
const MAX_FRAME_DEPTH: usize = 64;
const MAX_HEAP_OBJECTS: usize = 4096;

#[derive(Clone)]
enum Value {
    Int(i32),
    Text(String),
    Stream,
    Array(usize),
    Object(usize),
    Null,
}

enum HeapObject {
    Strings(Vec<Value>),
    StringBuilder(String),
}

struct Vm<'a> {
    class: &'a ClassFile,
    heap: Vec<HeapObject>,
    steps: usize,
    depth: usize,
}

pub fn execute_main(class: &ClassFile, args: &[&str]) -> Result<i32, &'static str> {
    let method = class
        .find_method("main", "([Ljava/lang/String;)V")
        .ok_or("main(String[]) method not found")?;
    let mut vm = Vm {
        class,
        heap: Vec::new(),
        steps: 0,
        depth: 0,
    };
    let array = vm.new_object(HeapObject::Strings(
        args.iter()
            .map(|arg| Value::Text(String::from(*arg)))
            .collect(),
    ))?;
    let result = vm.execute_method(method, &[Value::Array(array)])?;
    match result {
        Some(Value::Int(value)) => Ok(value),
        _ => Ok(0),
    }
}

impl<'a> Vm<'a> {
    fn new_object(&mut self, object: HeapObject) -> Result<usize, &'static str> {
        if self.heap.len() >= MAX_HEAP_OBJECTS {
            return Err("Java heap object limit exceeded");
        }
        let index = self.heap.len();
        self.heap.push(object);
        Ok(index)
    }

    fn execute_method(
        &mut self,
        method: &MethodInfo,
        arguments: &[Value],
    ) -> Result<Option<Value>, &'static str> {
        if self.depth >= MAX_FRAME_DEPTH {
            return Err("Java call-frame limit exceeded");
        }
        self.depth += 1;
        let result = self.execute_method_inner(method, arguments);
        self.depth -= 1;
        result
    }

    fn execute_method_inner(
        &mut self,
        method: &MethodInfo,
        arguments: &[Value],
    ) -> Result<Option<Value>, &'static str> {
        let mut locals = Vec::with_capacity(method.max_locals as usize);
        locals.resize(method.max_locals as usize, Value::Null);
        for (index, value) in arguments.iter().cloned().enumerate() {
            locals_set(&mut locals, index, value)?;
        }
        let mut stack: Vec<Value> = Vec::with_capacity(method.max_stack as usize);
        let code = &method.code;
        let mut pc = 0usize;

        while pc < code.len() {
            self.steps += 1;
            if self.steps > MAX_STEPS {
                return Err("Java VM step limit exceeded");
            }
            if self.steps % 1024 == 0 {
                crate::net::process_packets();
                if crate::shell::is_interrupted() {
                    crate::shell::clear_interrupt();
                    return Err("Interrupted");
                }
            }
            let op = code[pc];
            pc += 1;
            match op {
                0x00 => {}
                0x01 => push(&mut stack, Value::Null, method.max_stack)?,
                0x02..=0x08 => push(&mut stack, Value::Int((op as i32) - 0x03), method.max_stack)?,
                0x10 => push(
                    &mut stack,
                    Value::Int(read_i8(code, &mut pc)? as i32),
                    method.max_stack,
                )?,
                0x11 => push(
                    &mut stack,
                    Value::Int(read_i16(code, &mut pc)? as i32),
                    method.max_stack,
                )?,
                0x12 => {
                    let index = read_u8(code, &mut pc)? as u16;
                    self.push_constant(&mut stack, index, method.max_stack)?;
                }
                0x13 => {
                    let index = read_u16(code, &mut pc)?;
                    self.push_constant(&mut stack, index, method.max_stack)?;
                }
                0x1a..=0x1d => push(
                    &mut stack,
                    locals_get(&locals, (op - 0x1a) as usize)?,
                    method.max_stack,
                )?,
                0x2a..=0x2d => push(
                    &mut stack,
                    locals_get(&locals, (op - 0x2a) as usize)?,
                    method.max_stack,
                )?,
                0x3b..=0x3e => locals_set(&mut locals, (op - 0x3b) as usize, pop(&mut stack)?)?,
                0x4b..=0x4e => locals_set(&mut locals, (op - 0x4b) as usize, pop(&mut stack)?)?,
                0x2e => {
                    let index = pop_int(&mut stack)? as usize;
                    let array = pop(&mut stack)?;
                    let value = self.array_get(array, index)?;
                    push(&mut stack, value, method.max_stack)?;
                }
                0x32 => {
                    let index = pop_int(&mut stack)? as usize;
                    let array = pop(&mut stack)?;
                    let value = self.array_get(array, index)?;
                    push(&mut stack, value, method.max_stack)?;
                }
                0xbe => {
                    let array = pop(&mut stack)?;
                    let length = self.array_length(array)?;
                    push(&mut stack, Value::Int(length as i32), method.max_stack)?;
                }
                0x60 => binary_int(&mut stack, |a, b| a.wrapping_add(b), method.max_stack)?,
                0x64 => binary_int(&mut stack, |a, b| a.wrapping_sub(b), method.max_stack)?,
                0x68 => binary_int(&mut stack, |a, b| a.wrapping_mul(b), method.max_stack)?,
                0x6c => binary_int(
                    &mut stack,
                    |a, b| if b == 0 { 0 } else { a / b },
                    method.max_stack,
                )?,
                0x99..=0xa0 => {
                    let offset = read_i16(code, &mut pc)? as isize;
                    let branch = match op {
                        0x99 => pop_int(&mut stack)? == 0,
                        0x9a => pop_int(&mut stack)? != 0,
                        0x9b => pop_int(&mut stack)? < 0,
                        0x9c => pop_int(&mut stack)? >= 0,
                        0x9d => pop_int(&mut stack)? > 0,
                        0x9e => pop_int(&mut stack)? <= 0,
                        0x9f => {
                            let b = pop_int(&mut stack)?;
                            let a = pop_int(&mut stack)?;
                            a == b
                        }
                        0xa0 => {
                            let b = pop_int(&mut stack)?;
                            let a = pop_int(&mut stack)?;
                            a != b
                        }
                        _ => false,
                    };
                    if branch {
                        let base = pc.checked_sub(3).ok_or("Java branch underflow")?;
                        jump(&mut pc, offset, base, code.len())?;
                    }
                }
                0xb2 => {
                    let index = read_u16(code, &mut pc)?;
                    let (owner, name, _) = self.class.field_ref(index)?;
                    if owner == "java/lang/System" && name == "out" {
                        push(&mut stack, Value::Stream, method.max_stack)?;
                    } else {
                        return Err("Unsupported getstatic field");
                    }
                }
                0xbb => {
                    let index = read_u16(code, &mut pc)?;
                    if self.class.class_name(index)? != "java/lang/StringBuilder" {
                        return Err("Unsupported new object");
                    }
                    let object = self.new_object(HeapObject::StringBuilder(String::new()))?;
                    push(&mut stack, Value::Object(object), method.max_stack)?;
                }
                0x59 => {
                    let value = stack
                        .last()
                        .cloned()
                        .ok_or("Java operand stack underflow")?;
                    push(&mut stack, value, method.max_stack)?;
                }
                0xb6 => {
                    let index = read_u16(code, &mut pc)?;
                    let (owner, name, descriptor) = self.class.method_ref(index)?;
                    if owner == "java/io/PrintStream" && (name == "print" || name == "println") {
                        let value = pop(&mut stack)?;
                        let receiver = pop(&mut stack)?;
                        if !matches!(receiver, Value::Stream) {
                            return Err("PrintStream receiver is invalid");
                        }
                        print_value(value);
                        if name == "println" {
                            crate::println!();
                        }
                        let _ = descriptor;
                    } else if owner == "java/lang/StringBuilder" {
                        let count = parameter_count(descriptor)?;
                        if count > stack.len() {
                            return Err("Java operand stack underflow");
                        }
                        let start = stack.len() - count;
                        let arguments = stack.split_off(start);
                        let receiver = pop(&mut stack)?;
                        self.builder_call(
                            receiver,
                            name,
                            descriptor,
                            &arguments,
                            &mut stack,
                            method.max_stack,
                        )?;
                    } else {
                        return Err("Unsupported invokevirtual");
                    }
                }
                0xb7 => {
                    let index = read_u16(code, &mut pc)?;
                    let (_, name, _) = self.class.method_ref(index)?;
                    let receiver = pop(&mut stack)?;
                    if name != "<init>" || !matches!(receiver, Value::Object(_)) {
                        return Err("Unsupported invokespecial");
                    }
                }
                0xb8 => {
                    let index = read_u16(code, &mut pc)?;
                    let (owner, name, descriptor) = self.class.method_ref(index)?;
                    if owner != self.class_name()? {
                        return Err("Unsupported external invokestatic");
                    }
                    let target = self
                        .class
                        .find_method(name, descriptor)
                        .ok_or("Static method not found")?
                        .clone();
                    let count = parameter_count(descriptor)?;
                    if count > stack.len() {
                        return Err("Java operand stack underflow");
                    }
                    let start = stack.len() - count;
                    let arguments = stack.split_off(start);
                    if let Some(value) = self.execute_method(&target, &arguments)? {
                        push(&mut stack, value, method.max_stack)?;
                    }
                }
                0xac | 0xb0 => return Ok(Some(pop(&mut stack)?)),
                0xb1 => return Ok(None),
                _ => return Err("Unsupported Java bytecode"),
            }
        }
        Ok(None)
    }

    fn class_name(&self) -> Result<&str, &'static str> {
        self.class.class_name(self.class.this_class)
    }
    fn push_constant(
        &self,
        stack: &mut Vec<Value>,
        index: u16,
        max: u16,
    ) -> Result<(), &'static str> {
        let value = match self
            .class
            .constants
            .get(index as usize)
            .ok_or("Constant-pool index out of bounds")?
        {
            Constant::String(_) => Value::Text(String::from(self.class.string_constant(index)?)),
            Constant::Integer(_) => Value::Int(self.class.integer_constant(index)?),
            _ => return Err("Unsupported ldc constant"),
        };
        push(stack, value, max)
    }
    fn array_length(&self, value: Value) -> Result<usize, &'static str> {
        match value {
            Value::Array(index) => match self.heap.get(index) {
                Some(HeapObject::Strings(values)) => Ok(values.len()),
                _ => Err("Invalid array"),
            },
            _ => Err("arraylength on non-array"),
        }
    }
    fn array_get(&self, value: Value, index: usize) -> Result<Value, &'static str> {
        match value {
            Value::Array(array) => match self.heap.get(array) {
                Some(HeapObject::Strings(values)) => values
                    .get(index)
                    .cloned()
                    .ok_or("Java array index out of bounds"),
                _ => Err("Invalid array"),
            },
            _ => Err("aaload on non-array"),
        }
    }
    fn builder_call(
        &mut self,
        receiver: Value,
        name: &str,
        descriptor: &str,
        arguments: &[Value],
        stack: &mut Vec<Value>,
        max: u16,
    ) -> Result<(), &'static str> {
        let index = match receiver {
            Value::Object(index) => index,
            _ => return Err("StringBuilder receiver is invalid"),
        };
        let builder = match self.heap.get_mut(index) {
            Some(HeapObject::StringBuilder(value)) => value,
            _ => return Err("Invalid StringBuilder"),
        };
        if name == "append" {
            let argument = arguments.first().ok_or("StringBuilder argument missing")?;
            if descriptor == "(Ljava/lang/String;)Ljava/lang/StringBuilder;" {
                if let Value::Text(value) = argument {
                    builder.push_str(value);
                } else {
                    return Err("append expected String");
                }
            } else if descriptor == "(I)Ljava/lang/StringBuilder;" {
                if let Value::Int(value) = argument {
                    use core::fmt::Write;
                    let _ = write!(builder, "{}", value);
                } else {
                    return Err("append expected int");
                }
            } else {
                return Err("Unsupported StringBuilder.append");
            }
            push(stack, Value::Object(index), max)
        } else if name == "toString" && descriptor == "()Ljava/lang/String;" && arguments.is_empty()
        {
            push(stack, Value::Text(builder.clone()), max)
        } else {
            Err("Unsupported StringBuilder method")
        }
    }
}

fn parameter_count(descriptor: &str) -> Result<usize, &'static str> {
    let bytes = descriptor.as_bytes();
    if bytes.first() != Some(&b'(') {
        return Err("Invalid method descriptor");
    }
    let mut index = 1;
    let mut count = 0;
    while index < bytes.len() && bytes[index] != b')' {
        while bytes.get(index) == Some(&b'[') {
            index += 1;
        }
        if bytes.get(index) == Some(&b'L') {
            while bytes.get(index) != Some(&b';') {
                index += 1;
                if index >= bytes.len() {
                    return Err("Invalid method descriptor");
                }
            }
        }
        index += 1;
        count += 1;
    }
    if bytes.get(index) != Some(&b')') {
        return Err("Invalid method descriptor");
    }
    Ok(count)
}
fn print_value(value: Value) {
    match value {
        Value::Int(value) => crate::print!("{}", value),
        Value::Text(value) => crate::print!("{}", value),
        Value::Null => crate::print!("null"),
        Value::Stream => crate::print!("java.io.PrintStream"),
        Value::Array(_) => crate::print!("[array]"),
        Value::Object(_) => crate::print!("[object]"),
    }
}
fn push(stack: &mut Vec<Value>, value: Value, max: u16) -> Result<(), &'static str> {
    if stack.len() >= max as usize {
        return Err("Java operand stack overflow");
    }
    stack.push(value);
    Ok(())
}
fn binary_int(
    stack: &mut Vec<Value>,
    operation: fn(i32, i32) -> i32,
    max: u16,
) -> Result<(), &'static str> {
    let b = pop_int(stack)?;
    let a = pop_int(stack)?;
    push(stack, Value::Int(operation(a, b)), max)
}
fn pop(stack: &mut Vec<Value>) -> Result<Value, &'static str> {
    stack.pop().ok_or("Java operand stack underflow")
}
fn pop_int(stack: &mut Vec<Value>) -> Result<i32, &'static str> {
    match pop(stack)? {
        Value::Int(value) => Ok(value),
        _ => Err("Expected integer on operand stack"),
    }
}
fn locals_get(locals: &[Value], index: usize) -> Result<Value, &'static str> {
    locals
        .get(index)
        .cloned()
        .ok_or("Java local index out of bounds")
}
fn locals_set(locals: &mut [Value], index: usize, value: Value) -> Result<(), &'static str> {
    *locals
        .get_mut(index)
        .ok_or("Java local index out of bounds")? = value;
    Ok(())
}
fn read_u8(code: &[u8], pc: &mut usize) -> Result<u8, &'static str> {
    let value = *code.get(*pc).ok_or("Truncated Java bytecode")?;
    *pc += 1;
    Ok(value)
}
fn read_i8(code: &[u8], pc: &mut usize) -> Result<i8, &'static str> {
    Ok(read_u8(code, pc)? as i8)
}
fn read_u16(code: &[u8], pc: &mut usize) -> Result<u16, &'static str> {
    let hi = read_u8(code, pc)?;
    let lo = read_u8(code, pc)?;
    Ok(u16::from_be_bytes([hi, lo]))
}
fn read_i16(code: &[u8], pc: &mut usize) -> Result<i16, &'static str> {
    Ok(read_u16(code, pc)? as i16)
}
fn jump(pc: &mut usize, offset: isize, base: usize, code_len: usize) -> Result<(), &'static str> {
    let target = (base as isize)
        .checked_add(offset)
        .ok_or("Java branch overflow")?;
    if target < 0 || target as usize > code_len {
        return Err("Java branch out of bounds");
    }
    *pc = target as usize;
    Ok(())
}
