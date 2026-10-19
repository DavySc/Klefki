
### Check the signature

`SHA256SUMS.minisig` is a [minisign](https://jedisct1.github.io/minisign/) signature of `SHA256SUMS`, made with Klefki's release key. With both files in the download folder:

```
minisign -Vm SHA256SUMS -P {pubkey}
```

It prints `Signature and comment signature verified` for a genuine file; then check your download against `SHA256SUMS` as above. The same public key is in the repository as `minisign.pub`.
