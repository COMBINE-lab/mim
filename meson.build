project('mimindex', 'cpp',
  version : '1.0.0',
  default_options : [
    'warning_level=3',
    'cpp_std=c++17',
    'optimization=3',
    'debug=true'
  ]
)

# Find dependencies
zlib_dep = dependency('zlib', required : true, fallback : ['zlib', 'zlib_dep'])
thread_dep = dependency('threads', required : true)
blake3_dep = dependency('blake3', required : true, fallback : ['blake3', 'blake3_dep'])

# Get blake3 include directories from the subproject
# This is needed because dependency include_directories don't always propagate
blake3_subproj = subproject('blake3')
blake3_inc = blake3_subproj.get_variable('blake3_inc_dir')

# Include directories
inc_dirs = include_directories('.', 'include')

# mimindex executable
mimindex_exe = executable('mimindex',
  'src/main.cpp',
  include_directories : [inc_dirs, blake3_inc],
  dependencies : [zlib_dep, blake3_dep],
  install : true
)

# test_mim_parser executable
test_mim_parser_exe = executable('test_mim_parser',
  'src/test_parser.cpp',
  include_directories : [inc_dirs, blake3_inc],
  dependencies : [zlib_dep, thread_dep, blake3_dep],
  install : false
)

# test_serial_parser executable
test_serial_parser_exe = executable('test_serial_parser',
  'src/test_serial_parser.cpp',
  include_directories : [inc_dirs, blake3_inc],
  dependencies : [zlib_dep, thread_dep, blake3_dep],
  install : false
)

# Optional: Add sanitizer build option (commented out by default)
# To enable: meson configure -Db_sanitize=address
# if get_option('b_sanitize') == 'address'
#   add_project_arguments('-fsanitize=address', language : 'cpp')
#   add_project_link_arguments('-fsanitize=address', language : 'cpp')
# endif
