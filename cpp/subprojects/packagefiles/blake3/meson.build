project('blake3', 'c',
  version: '1.8.0',
  license: 'CC0-1.0 OR Apache-2.0',
  default_options: ['c_std=c99', 'warning_level=2']
)

cc = meson.get_compiler('c')
host_system = host_machine.system()
host_cpu_family = host_machine.cpu_family()

# Base source files
blake3_sources = [
  'c/blake3.c',
  'c/blake3_dispatch.c',
  'c/blake3_portable.c',
]

# Platform-specific configuration
blake3_c_args = []
blake3_link_args = []

# x86/x64 SIMD implementations
if host_cpu_family == 'x86' or host_cpu_family == 'x86_64'
  # Determine which assembly flavor to use
  is_windows = host_system == 'windows'
  is_msvc = cc.get_id() == 'msvc'
  
  if is_windows and is_msvc
    asm_ext = '_x86-64_windows_msvc.asm'
  elif is_windows
    asm_ext = '_x86-64_windows_gnu.S'
  else
    asm_ext = '_x86-64_unix.S'
  endif
  
  # Add SIMD assembly files
  blake3_sources += [
    'c/blake3_sse2' + asm_ext,
    'c/blake3_sse41' + asm_ext,
    'c/blake3_avx2' + asm_ext,
    'c/blake3_avx512' + asm_ext,
  ]
  
  # For MSVC, we need special handling
  if is_msvc
    # MSVC assembly files need to be compiled with MASM
    # This is handled automatically by Meson for .asm files
  endif
  
# ARM NEON implementation
elif host_cpu_family == 'arm' or host_cpu_family == 'aarch64'
  blake3_sources += ['c/blake3_neon.c']
  
  # Check if NEON is available
  if cc.has_argument('-mfpu=neon')
    blake3_c_args += ['-mfpu=neon']
  endif
endif

# Build the library
# Sources are in c/ subdirectory, and they #include "blake3.h" and "blake3_impl.h"
# So we need to make 'c' the include directory for building
blake3_lib = static_library('blake3',
  blake3_sources,
  c_args: blake3_c_args,
  link_args: blake3_link_args,
  include_directories: include_directories('c'),
  install: false
)

# Create dependency object
# Export the 'c' directory so users can #include <blake3.h>
blake3_inc_dir = include_directories('c')
blake3_dep = declare_dependency(
  link_with: blake3_lib,
  include_directories: blake3_inc_dir,
  version: meson.project_version()
)

# Make it available as the provided dependency name
meson.override_dependency('blake3', blake3_dep)

# Also create a pkgconfig file for external use (optional)
pkg = import('pkgconfig')
pkg.generate(blake3_lib,
  name: 'blake3',
  description: 'BLAKE3 cryptographic hash function',
  version: meson.project_version(),
  subdirs: 'c'
)
