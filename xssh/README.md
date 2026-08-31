# xssh

SSH with private keys stored encrypted in S3. Keys are encrypted client-side
(argon2id + ChaCha20-Poly1305) before upload, fetched and decrypted in memory
at connect time, and freed the moment authentication succeeds. Nothing is ever
written to disk and the bucket only ever sees ciphertext.

## How it works

1. `xssh add-key` encrypts a private key with a key derived from your
   password (argon2id with a random per-key salt) and uploads it to an S3
   bucket (MinIO or any S3-compatible store). Each key gets its own salt,
   stored alongside the ciphertext, so no two keys ever share a derived key.
   `xssh gen-key` does the same with a freshly generated ed25519 key that
   never exists outside process memory, and `xssh pub-key` prints the
   public key of a stored key (for `authorized_keys`) without writing
   anything to disk.
2. `xssh connect <alias>` fetches the encrypted key, prompts for your
   password and decrypts it in memory.
3. The key goes into an anonymous in-memory file (`memfd_create`) and
   `ssh -i` is pointed at it. It never touches the filesystem.
4. As soon as ssh authenticates, the in-memory key is destroyed. Your session
   keeps running normally.

## Install

```bash
cargo build --release
sudo cp target/release/xssh /usr/local/bin/
```

Or grab a binary from the releases page.

## MinIO setup

Create the bucket and a user that can only touch that bucket (the policy is
in `policy.json`, scoped to the `ssh-keys` bucket):

```bash
mc mb myminio/ssh-keys
mc admin user add myminio xssh 'ChangeMePlease'
mc admin policy create myminio xssh-keys-rw policy.json
mc admin policy attach myminio xssh-keys-rw --user xssh
```

Use that user's credentials in the config below. If you rename the bucket,
update the `Resource` ARNs in `policy.json` to match.

## Configure

Create `~/.secrets/sssh/config.yaml` (see `config.yaml.example`):

```yaml
endpoint: "https://s3.example.com"   # S3 or MinIO endpoint
bucket: "ssh-keys"                   # bucket holding the encrypted keys
minio_user: "myuser"                 # S3 access key
minio_secret: "mysecret"             # S3 secret key
```

Keep the file `chmod 600` — it holds the S3 secret. Aliases are added by
`add-key`. The argon2 salt lives inside each encrypted key, not in the config.

## Use

Add a key (encrypts it locally, uploads it, creates the alias):

```bash
xssh add-key myserver root@203.0.113.10 myserver ~/.ssh/id_ed25519
```

Or generate a brand-new ed25519 key that never exists outside memory — it is
created in-process, encrypted and uploaded without ever touching disk:

```bash
xssh gen-key myserver root@203.0.113.10 myserver
```

Connect:

```bash
xssh connect myserver
xssh c myserver -L 8080:localhost:8080   # extra args are passed to ssh
```

Print the public key for a stored private key (fetches and decrypts it in
memory, derives the public key in-process, never writes to disk):

```bash
xssh pub-key myserver
```

List keys (aliases from the config cross-checked against the bucket — also
shows objects with no alias and aliases whose object is gone):

```bash
xssh list-keys
```

Delete a key (removes the encrypted object from the bucket and the alias
from the config — asks you to type the alias back, since the bucket holds
the only copy):

```bash
xssh del-key myserver
```

Every command has shortcuts:

| Command     | Shortcuts        |
|-------------|------------------|
| `connect`   | `con`, `c`       |
| `add-key`   | `add`, `a`       |
| `gen-key`   | `gen`, `g`       |
| `pub-key`   | `pub`, `p`       |
| `del-key`   | `del`, `d`       |
| `list-keys` | `list`, `ls`, `l`|

You are prompted for the encryption password on all of these (except
`del-key` and `list-keys`). Command names and their shortcuts are reserved
and cannot be used as alias names.

## Notes

- Linux only.
- Runs your real `ssh`, so options, config and behavior all work as usual.
- To watch the key appear and disappear from memory:

  ```bash
  watch -n 0.2 "find /proc/[0-9]*/fd -lname '/memfd:xssh-key*' 2>/dev/null"
  ```
