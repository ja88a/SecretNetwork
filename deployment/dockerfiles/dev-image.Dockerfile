# Final image
FROM build-release

COPY deployment/docker/devimage/bootstrap_init_no_stop.sh bootstrap_init.sh
COPY deployment/docker/devimage/faucet/faucet_server.js .

HEALTHCHECK --interval=5s --timeout=1s CMD bash -c 'curl -sf http://localhost:26657/status || echo '{"result":{"sync_info":{"latest_block_height":"0"}}}' | jq -e ".result.sync_info.latest_block_height | tonumber > 0" > /dev/null'

ENTRYPOINT ["./bootstrap_init.sh"]