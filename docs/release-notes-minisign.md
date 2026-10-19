First check `SHA256SUMS` itself, then your files against it. `SHA256SUMS.minisig` is a [minisign](https://jedisct1.github.io/minisign/) signature of `SHA256SUMS`, made with Klefki's release key (also in the repository as `minisign.pub`). With both files in the download folder:

```
minisign -Vm SHA256SUMS -P {pubkey}
```

It must print `Signature and comment signature verified` and `Trusted comment: Klefki SHA256SUMS, v{version}, …`. If the trusted comment names another version, you have the checksums of a different release: do not use them.

