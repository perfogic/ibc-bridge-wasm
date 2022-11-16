## Installation

```bash

# run the build container for oraid. Wait for it to finish
docker-compose -f docker-compose.build.yml up
```

## Start the networks

```bash
docker-compose up -d
```

## deploy smart contract

```bash
# build smart contract
./scripts/build_contract.sh contracts/cw-ics20-latest
cp contracts/cw-ics20-latest/artifacts/cw-ics20-latest.wasm .mars

# build cw20
./scripts/build_contract.sh contracts/cw20-base
cp contracts/cw20-base/artifacts/cw20-base.wasm .mars

# build receiver contract
./scripts/build_contract.sh contracts/receiver-contract
cp contracts/receiver-contract/artifacts/receiver-contract.wasm .mars

# go to mars network
docker-compose exec mars ash

./scripts/deploy_contract.sh .mars/receiver-contract.wasm 'receiver-contract' '{}'
# mars14hj2tavq8fpesdwxxcu44rty3hh90vhujrvcmstl4zr3txmfvw9smxjtde

./scripts/deploy_contract.sh .mars/cw-ics20-latest.wasm 'cw20-ics20-latest' '{"default_timeout":180,"gov_contract":"mars15ez8l0c2qte2sa0a4xsdmaswy96vzj2fl2ephq","allowlist":[],"native_allow_contract":"mars14hj2tavq8fpesdwxxcu44rty3hh90vhujrvcmstl4zr3txmfvw9smxjtde"}'
# mars1nc5tatafv6eyq7llkr2gv50ff9e22mnf70qgjlv737ktmt4eswrqhnhf0l

./scripts/deploy_contract.sh .mars/cw20-base.wasm 'cw20-base' '{"name":"EARTH","symbol":"EARTH","decimals":6,"initial_balances":[{"address":"mars14hj2tavq8fpesdwxxcu44rty3hh90vhujrvcmstl4zr3txmfvw9smxjtde","amount":"100000000000000"}],"mint":{"minter":"mars15ez8l0c2qte2sa0a4xsdmaswy96vzj2fl2ephq"}}'
# mars17p9rzwnnfxcjp32un9ug7yhhzgtkhvl9jfksztgw5uh69wac2pgs0gfvxm

# mint token for cw20-ics20 (optional)
oraid tx wasm execute mars17p9rzwnnfxcjp32un9ug7yhhzgtkhvl9jfksztgw5uh69wac2pgs0gfvxm '{"mint":{"recipient":"mars14hj2tavq8fpesdwxxcu44rty3hh90vhujrvcmstl4zr3txmfvw9smxjtde","amount":"100000000000000000000000"}}' --keyring-backend test --from $USER --chain-id $CHAIN_ID -y

oraid tx wasm execute mars1nc5tatafv6eyq7llkr2gv50ff9e22mnf70qgjlv737ktmt4eswrqhnhf0l '{"update_cw20_mapping_pair":{"src_ibc_endpoint":{"port_id":"transfer","channel_id":"channel-0"},"dest_ibc_endpoint":{"port_id":"wasm.mars1nc5tatafv6eyq7llkr2gv50ff9e22mnf70qgjlv737ktmt4eswrqhnhf0l","channel_id":"channel-0"},"denom":"earth","cw20_denom":"cw20:mars17p9rzwnnfxcjp32un9ug7yhhzgtkhvl9jfksztgw5uh69wac2pgs0gfvxm","remote_decimals":6}}' --from $USER --chain-id $CHAIN_ID -y --keyring-backend test -b block


# migrate contract
./scripts/migrate_contract.sh .mars/cw20-ics20.wasm mars1wug8sewp6cedgkmrmvhl3lf3tulagm9hnvy8p0rppz9yjw0g4wtqrhq5mn # migrate to test changing cw20 contract
```

## start relayer

```bash

docker-compose exec hermes bash
hermes --config config.toml keys add --chain Earth --mnemonic-file accounts/Earth.txt
hermes --config config.toml keys add --chain Mars --mnemonic-file accounts/Mars.txt

# create a channel
hermes --config config.toml create channel --a-chain Earth --b-chain Mars --a-port transfer --b-port wasm.mars1nc5tatafv6eyq7llkr2gv50ff9e22mnf70qgjlv737ktmt4eswrqhnhf0l --new-client-connection

# start hermes
hermes --config config.toml start
```

## send cross-channel

```bash
# from earth to mars on channel
docker compose exec earth ash
oraid tx ibc-transfer transfer transfer channel-0 mars15ez8l0c2qte2sa0a4xsdmaswy96vzj2fl2ephq 10000000earth --from duc --chain-id Earth -y --keyring-backend test
# check mars balance
docker compose exec mars ash
oraid query wasm contract-state smart mars18vd8fpwxzck93qlwghaj6arh4p7c5n89plpqv0 '{"balance":{"address":"mars15ez8l0c2qte2sa0a4xsdmaswy96vzj2fl2ephq"}}'

# from mars to earth send back
# send back command
oraid tx wasm execute mars18vd8fpwxzck93qlwghaj6arh4p7c5n89plpqv0 '{"send":{"amount":"10000000","contract":"mars10pyejy66429refv3g35g2t7am0was7ya90pn2w","msg":"'$(echo '{"channel":"channel-0","remote_address":"earth1w84gt7t7dzvj6qmf5q73d2yzyz35uwc7y8fkwp"}' | base64 -w 0)'"}}' --from $USER --chain-id $CHAIN_ID -y --keyring-backend test
```

# TODO:

remove hard code & update dynamic logic for cw20-ics20. Now the demo is for prototype only (proof of concept)