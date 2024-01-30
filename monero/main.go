package main

import (
	"context"
	"fmt"
	"log"
	"net/http"

	"github.com/gabstv/httpdigest"
	"gitlab.com/moneropay/go-monero/walletrpc"
)

// ./monero-wallet-rpc --rpc-bind-port 18083 --wallet-file /home/keyb1nd/Downloads/monero-gui/monero-storage/wallets/nicelandvpn/nicelandvpn --password password^C-rpc-login test:test

func main() {
	// username: kernal, password: s3cure
	client := walletrpc.New(walletrpc.Config{
		Address: "http://127.0.0.1:18083/json_rpc",
		Client: &http.Client{
			Transport: httpdigest.New("test", "test"), // Remove if no auth.
		},
	})
	resp, err := client.GetBalance(context.Background(), &walletrpc.GetBalanceRequest{})
	if err != nil {
		log.Println(err)
	}

	fmt.Println("Total balance:", walletrpc.XMRToDecimal(resp.Balance))
	fmt.Println("Unlocked balance:", walletrpc.XMRToDecimal(resp.UnlockedBalance))

	resp2, err2 := client.GetAddress(context.Background(), &walletrpc.GetAddressRequest{})
	if err2 != nil {
		log.Println(err2)
	}
	log.Println(resp2, err2)

	resp3, err3 := client.GetTransfers(context.Background(), &walletrpc.GetTransfersRequest{
		In:           true,
		AccountIndex: 0,
	})
	if err3 != nil {
		log.Println(err3)
	}

	log.Println("PRE")
	for i, v := range resp3.In {
		fmt.Println("TX:", i, walletrpc.XMRToDecimal(v.Amount), v.Note, walletrpc.XMRToDecimal(v.Fee), v.Txid, v.PaymentId)
		// respX, errX := client.GetPayments(context.Background(), &walletrpc.GetPaymentsRequest{
		// 	PaymentId: v.PaymentId,
		// })
		// if errX != nil {
		// 	log.Println(errX)
		// }
		// log.Println(respX.Payments[0].Amount)
	}

	// x, e := client.MakeIntegratedAddress(context.Background(), &walletrpc.MakeIntegratedAddressRequest{
	// 	PaymentId:       walletrpc.NewPaymentID64(),
	// 	StandardAddress: "43GGa2DezEdWdRNALRy4fMAceAGThNMeuKWNH1VGtD7nA4mXFwqgAjMW4VWxjCi85qDev3LxBu8Bq24S9hyprDpqV7qzXwV",
	// })
	// log.Println(x.PaymentId, x.IntegratedAddress, e)

}
