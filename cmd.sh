
./mc rb --force --dangerous s1s/test --insecure
./mc mb s1s/test --insecure

./mc ls s1s/test --insecure

./mc cp --encrypt="s1s/test/=cli-key" tf s1s/test/tfkms --insecure
./mc cp --encrypt="s1s/test/=minio-key" tf s1s/test/tfkms --insecure
./mc stat s1s/test/tfkms --insecure
./mc cp s1s/test/tfkms tfkms --insecure

./mc cp --encrypt-key="s1s/test/=12345678901234567890123456789011" tf s1s/test/tfc1 --insecure
./mc stat --encrypt-key="s1s/test/=12345678901234567890123456789011" s1s/test/tfc1 --insecure

./mc cp --encrypt-key="s1s/test/=12345678901234567890123456789022" tf s1s/test/tfc2 --insecure
./mc stat --encrypt-key="s1s/test/=12345678901234567890123456789022" s1s/test/tfc2 --insecure

./mc cp s1s/test/tfc1 tfc1 --insecure
./mc cp s1s/test/tfc2 tfc2 --insecure
