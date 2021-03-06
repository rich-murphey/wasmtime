//! ARM 64-bit Instruction Set Architecture.

use crate::ir::Function;
use crate::isa::Builder as IsaBuilder;
use crate::machinst::{
    compile, MachBackend, MachCompileResult, ShowWithRRU, TargetIsaAdapter, VCode,
};
use crate::result::CodegenResult;
use crate::settings;

use alloc::boxed::Box;

use regalloc::RealRegUniverse;
use target_lexicon::{Aarch64Architecture, Architecture, Triple};

// New backend:
mod abi;
pub(crate) mod inst;
mod lower;
mod lower_inst;

use inst::create_reg_universe;

/// An AArch64 backend.
pub struct AArch64Backend {
    triple: Triple,
    flags: settings::Flags,
    reg_universe: RealRegUniverse,
}

impl AArch64Backend {
    /// Create a new AArch64 backend with the given (shared) flags.
    pub fn new_with_flags(triple: Triple, flags: settings::Flags) -> AArch64Backend {
        let reg_universe = create_reg_universe(&flags);
        AArch64Backend {
            triple,
            flags,
            reg_universe,
        }
    }

    /// This performs lowering to VCode, register-allocates the code, computes block layout and
    /// finalizes branches. The result is ready for binary emission.
    fn compile_vcode(
        &self,
        func: &Function,
        flags: settings::Flags,
    ) -> CodegenResult<VCode<inst::Inst>> {
        let abi = Box::new(abi::AArch64ABIBody::new(func, flags)?);
        compile::compile::<AArch64Backend>(func, self, abi)
    }
}

impl MachBackend for AArch64Backend {
    fn compile_function(
        &self,
        func: &Function,
        want_disasm: bool,
    ) -> CodegenResult<MachCompileResult> {
        let flags = self.flags();
        let vcode = self.compile_vcode(func, flags.clone())?;
        let buffer = vcode.emit();
        let frame_size = vcode.frame_size();

        let disasm = if want_disasm {
            Some(vcode.show_rru(Some(&create_reg_universe(flags))))
        } else {
            None
        };

        let buffer = buffer.finish();

        Ok(MachCompileResult {
            buffer,
            frame_size,
            disasm,
        })
    }

    fn name(&self) -> &'static str {
        "aarch64"
    }

    fn triple(&self) -> Triple {
        self.triple.clone()
    }

    fn flags(&self) -> &settings::Flags {
        &self.flags
    }

    fn reg_universe(&self) -> &RealRegUniverse {
        &self.reg_universe
    }
}

/// Create a new `isa::Builder`.
pub fn isa_builder(triple: Triple) -> IsaBuilder {
    assert!(triple.architecture == Architecture::Aarch64(Aarch64Architecture::Aarch64));
    IsaBuilder {
        triple,
        setup: settings::builder(),
        constructor: |triple, shared_flags, _| {
            let backend = AArch64Backend::new_with_flags(triple, shared_flags);
            Box::new(TargetIsaAdapter::new(backend))
        },
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::cursor::{Cursor, FuncCursor};
    use crate::ir::types::*;
    use crate::ir::{AbiParam, ExternalName, Function, InstBuilder, Signature};
    use crate::isa::CallConv;
    use crate::settings;
    use crate::settings::Configurable;
    use core::str::FromStr;
    use target_lexicon::Triple;

    #[test]
    fn test_compile_function() {
        let name = ExternalName::testcase("test0");
        let mut sig = Signature::new(CallConv::SystemV);
        sig.params.push(AbiParam::new(I32));
        sig.returns.push(AbiParam::new(I32));
        let mut func = Function::with_name_signature(name, sig);

        let bb0 = func.dfg.make_block();
        let arg0 = func.dfg.append_block_param(bb0, I32);

        let mut pos = FuncCursor::new(&mut func);
        pos.insert_block(bb0);
        let v0 = pos.ins().iconst(I32, 0x1234);
        let v1 = pos.ins().iadd(arg0, v0);
        pos.ins().return_(&[v1]);

        let mut shared_flags = settings::builder();
        shared_flags.set("opt_level", "none").unwrap();
        let backend = AArch64Backend::new_with_flags(
            Triple::from_str("aarch64").unwrap(),
            settings::Flags::new(shared_flags),
        );
        let buffer = backend.compile_function(&mut func, false).unwrap().buffer;
        let code = &buffer.data[..];

        // stp x29, x30, [sp, #-16]!
        // mov x29, sp
        // mov x1, #0x1234
        // add w0, w0, w1
        // mov sp, x29
        // ldp x29, x30, [sp], #16
        // ret
        let golden = vec![
            0xfd, 0x7b, 0xbf, 0xa9, 0xfd, 0x03, 0x00, 0x91, 0x81, 0x46, 0x82, 0xd2, 0x00, 0x00,
            0x01, 0x0b, 0xbf, 0x03, 0x00, 0x91, 0xfd, 0x7b, 0xc1, 0xa8, 0xc0, 0x03, 0x5f, 0xd6,
        ];

        assert_eq!(code, &golden[..]);
    }

    #[test]
    fn test_branch_lowering() {
        let name = ExternalName::testcase("test0");
        let mut sig = Signature::new(CallConv::SystemV);
        sig.params.push(AbiParam::new(I32));
        sig.returns.push(AbiParam::new(I32));
        let mut func = Function::with_name_signature(name, sig);

        let bb0 = func.dfg.make_block();
        let arg0 = func.dfg.append_block_param(bb0, I32);
        let bb1 = func.dfg.make_block();
        let bb2 = func.dfg.make_block();
        let bb3 = func.dfg.make_block();

        let mut pos = FuncCursor::new(&mut func);
        pos.insert_block(bb0);
        let v0 = pos.ins().iconst(I32, 0x1234);
        let v1 = pos.ins().iadd(arg0, v0);
        pos.ins().brnz(v1, bb1, &[]);
        pos.ins().jump(bb2, &[]);
        pos.insert_block(bb1);
        pos.ins().brnz(v1, bb2, &[]);
        pos.ins().jump(bb3, &[]);
        pos.insert_block(bb2);
        let v2 = pos.ins().iadd(v1, v0);
        pos.ins().brnz(v2, bb2, &[]);
        pos.ins().jump(bb1, &[]);
        pos.insert_block(bb3);
        let v3 = pos.ins().isub(v1, v0);
        pos.ins().return_(&[v3]);

        let mut shared_flags = settings::builder();
        shared_flags.set("opt_level", "none").unwrap();
        let backend = AArch64Backend::new_with_flags(
            Triple::from_str("aarch64").unwrap(),
            settings::Flags::new(shared_flags),
        );
        let result = backend
            .compile_function(&mut func, /* want_disasm = */ false)
            .unwrap();
        let code = &result.buffer.data[..];

        // stp	x29, x30, [sp, #-16]!
        // mov	x29, sp
        // mov	x1, #0x1234                	// #4660
        // add	w0, w0, w1
        // mov	w1, w0
        // cbnz	x1, 0x28
        // mov	x1, #0x1234                	// #4660
        // add	w1, w0, w1
        // mov	w1, w1
        // cbnz	x1, 0x18
        // mov	w1, w0
        // cbnz	x1, 0x18
        // mov	x1, #0x1234                	// #4660
        // sub	w0, w0, w1
        // mov	sp, x29
        // ldp	x29, x30, [sp], #16
        // ret
        let golden = vec![
            253, 123, 191, 169, 253, 3, 0, 145, 129, 70, 130, 210, 0, 0, 1, 11, 225, 3, 0, 42, 161,
            0, 0, 181, 129, 70, 130, 210, 1, 0, 1, 11, 225, 3, 1, 42, 161, 255, 255, 181, 225, 3,
            0, 42, 97, 255, 255, 181, 129, 70, 130, 210, 0, 0, 1, 75, 191, 3, 0, 145, 253, 123,
            193, 168, 192, 3, 95, 214,
        ];

        assert_eq!(code, &golden[..]);
    }
}
