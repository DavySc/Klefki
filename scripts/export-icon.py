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
art(256).save(f"{D}/klefki-256.png",optimize=True)
# Windows asks for 16, 20, 24, 30, 32, 36, 40, 48, 64 and 256 px depending on display scaling.
sizes=[16,20,24,30,32,36,40,48,64,128,256]
blobs=[pngb(art(s)) for s in sizes]
out=struct.pack('<HHH',0,1,len(sizes)); off=6+16*len(sizes)
for s,b in zip(sizes,blobs):
    out+=struct.pack('<BBBBHHII',s%256,s%256,0,0,1,32,len(b),off); off+=len(b)
open(f"{D}/klefki.ico",'wb').write(out+b''.join(blobs))
# icns by hand: Pillow leaves out the 16 and 32 px (@1x) entries. Each entry is a type, a length and a PNG.
icns=[("icp4",16),("ic11",32),("icp5",32),("ic12",64),("icp6",64),("ic07",128),("ic13",256),("ic08",256),("ic14",512),("ic09",512),("ic10",1024)]
body=b"".join(t.encode()+struct.pack(">I",8+len(b))+b for t,b in ((t,pngb(art(s))) for t,s in icns))
open(f"{D}/klefki.icns","wb").write(b"icns"+struct.pack(">I",8+len(body))+body)
