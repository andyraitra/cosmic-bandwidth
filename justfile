name := 'cosmic-bandwidth'
appid := 'io.github.andyraitra.CosmicBandwidth'

prefix := env('HOME') / '.local'
bin-dst := prefix / 'bin' / name
desktop-dst := prefix / 'share' / 'applications' / appid + '.desktop'

build:
    cargo build --release

test:
    cargo test

# Install for the current user, no sudo needed
install: build
    install -Dm0755 target/release/{{name}} {{bin-dst}}
    install -Dm0644 data/{{appid}}.desktop {{desktop-dst}}

uninstall:
    rm -f {{bin-dst}} {{desktop-dst}}
