DST = /home/sebk/data/view_wasm

build:
	wasm-pack build -t no-modules --release
	cp pkg/pdf_view.js pkg/pdf_view_bg.wasm $(DST)/pkg/
	cp ../wasm/* $(DST)/

publish:
	git -C $(DST) commit -a -m "update"
	git -C $(DST) push

.PHONY:	all

all:	build
