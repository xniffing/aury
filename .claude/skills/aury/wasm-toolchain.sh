# wasm32-wasi toolchain detection for the Aury skill. Sourced by dev.sh (wasm
# parity stage) and wasm-lib.sh (reactor builder), and reusable by any project.
# Source this (do not execute it) to export the env `aury wasm`/`wasm-lib` need:
#   AURY_WASM_CLANG  clang with the WebAssembly target
#   WASI_SYSROOT     wasi-libc sysroot
#   PATH             prepended with the directory holding `wasm-ld`
#
# Resolution order: honour anything already set, then a wasi-sdk install
# (self-contained, auto-detected by `aury` itself), then a Homebrew assembly
# (llvm + lld + wasi-libc). If none is found the variables are left unset and
# `aury wasm` will emit its own guidance.

# 1. wasi-sdk: `aury` auto-detects WASI_SDK_PATH / /opt/wasi-sdk. Just make sure
#    its bin (with wasm-ld) is reachable.
_aury_wasi_sdk="${WASI_SDK_PATH:-/opt/wasi-sdk}"
if [[ -x "$_aury_wasi_sdk/bin/clang" ]]; then
  export WASI_SDK_PATH="$_aury_wasi_sdk"
  case ":$PATH:" in
    *":$_aury_wasi_sdk/bin:"*) ;;
    *) export PATH="$_aury_wasi_sdk/bin:$PATH" ;;
  esac
# 2. Homebrew: llvm (clang + wasm target), lld (wasm-ld), wasi-libc (sysroot).
elif command -v brew >/dev/null 2>&1; then
  _aury_llvm="$(brew --prefix llvm 2>/dev/null)"
  _aury_lld="$(brew --prefix lld 2>/dev/null)"
  _aury_wasi="$(brew --prefix wasi-libc 2>/dev/null)"
  if [[ -x "$_aury_llvm/bin/clang" && -x "$_aury_lld/bin/wasm-ld" && -d "$_aury_wasi/share/wasi-sysroot" ]]; then
    export AURY_WASM_CLANG="${AURY_WASM_CLANG:-$_aury_llvm/bin/clang}"
    export WASI_SYSROOT="${WASI_SYSROOT:-$_aury_wasi/share/wasi-sysroot}"
    case ":$PATH:" in
      *":$_aury_lld/bin:"*) ;;
      *) export PATH="$_aury_lld/bin:$PATH" ;;
    esac
  fi
fi
unset _aury_wasi_sdk _aury_llvm _aury_lld _aury_wasi
