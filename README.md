# Ariel OS firmware for Winky


## Sample server

Create a simple server using [json-server](https://github.com/typicode/json-server):

```
cp db.json5.sample db.json5
json-server --host 0.0.0.0 db.json5  
```


Run the firmware:
```
CONFIG_WIFI_NETWORK=<wi-fi network name> CONFIG_WIFI_PASSWORD=<wi-fi password> ENDPOINT_URL=http://<ip of the pc hosting the server>:3000/report laze build run 
```
