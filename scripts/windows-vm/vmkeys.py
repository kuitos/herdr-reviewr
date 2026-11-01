#!/usr/bin/env python3
"""Type into the Windows QA VM through QEMU's monitor socket (VM_DIR, default ~/VMs/reviewr-windows).
   vmkeys.py "text" [--enter]    types text
   vmkeys.py --key meta_l-r      sends one QEMU key combo"""
import os
import socket, sys, time
MAP = {' ':'spc','\n':'ret','.':'dot',',':'comma','/':'slash','\\':'backslash','-':'minus','=':'equal',';':'semicolon',"'":'apostrophe','[':'bracket_left',']':'bracket_right','`':'grave_accent'}
SHIFT = {':':'semicolon','"':'apostrophe','_':'minus','+':'equal','?':'slash','|':'backslash','{':'bracket_left','}':'bracket_right','<':'comma','>':'dot','~':'grave_accent','!':'1','@':'2','#':'3','$':'4','%':'5','^':'6','&':'7','*':'8','(':'9',')':'0'}
def send(cmd):
    s = socket.socket(socket.AF_UNIX); s.connect(os.path.join(os.environ.get('VM_DIR', os.path.expanduser('~/VMs/reviewr-windows')), 'monitor.sock')); s.recv(4096)
    s.sendall((cmd + '\n').encode()); time.sleep(0.04); s.close()
args = sys.argv[1:]
if args[0] == '--key':
    send('sendkey ' + args[1]); sys.exit()
for ch in args[0]:
    if ch.isalpha(): send('sendkey ' + ('shift-' if ch.isupper() else '') + ch.lower())
    elif ch.isdigit(): send('sendkey ' + ch)
    elif ch in MAP: send('sendkey ' + MAP[ch])
    elif ch in SHIFT: send('sendkey shift-' + SHIFT[ch])
if '--enter' in args: send('sendkey ret')
