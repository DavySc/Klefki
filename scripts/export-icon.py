"""Re-export the app icon from its SVGs: the PNGs the app and README use, a Windows .ico and a macOS .icns.

Run from the repository root after editing crates/app/assets/icon/*.svg:
    python3 -m pip install cairosvg pillow
    python3 scripts/export-icon.py
16 and 24 px use klefki-small.svg (bigger shapes, no spokes); every other size uses klefki.svg.
"""
import cairosvg, io, struct, sys
from PIL import Image
D=sys.argv[1] if len(sys.argv) > 1 else "crates/app/assets/icon"
def img(f,s): return Image.open(io.BytesIO(cairosvg.svg2png(url=f"{D}/{f}",output_width=s,output_height=s))).convert('RGBA')
def art(s): return img('klefki-small.svg' if s<=24 else 'klefki.svg', s)
def pngb(im):
    b=io.BytesIO(); im.save(b,'PNG',optimize=True); return b.getvalue()
for s in (128,256): art(s).save(f"{D}/klefki-{s}.png",optimize=True)
sizes=[16,24,32,48,64,128,256]
blobs=[pngb(art(s)) for s in sizes]
out=struct.pack('<HHH',0,1,len(sizes)); off=6+16*len(sizes)
for s,b in zip(sizes,blobs):
    out+=struct.pack('<BBBBHHII',s%256,s%256,0,0,1,32,len(b),off); off+=len(b)
open(f"{D}/klefki.ico",'wb').write(out+b''.join(blobs))
big=art(1024); big.save(f"{D}/klefki.icns",append_images=[art(s) for s in (16,32,64,128,256,512)])
