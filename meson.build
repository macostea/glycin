project(
    'glycin',
    'rust',
    version: '0.0.1',
    meson_version: '>=0.57',
)

dependency('gtk4', version: '>= 4.11.2')

i18n = import ('i18n')

compat_version = '0+'

libexecdir = get_option('prefix') / get_option('libexecdir') / meson.project_name() / compat_version
datadir = get_option('prefix') / get_option('datadir') / meson.project_name() / compat_version

subdir('loaders')