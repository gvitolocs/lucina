use alloc::boxed::Box;

use usb_if::{
    descriptor::{ConfigurationDescriptor, DescriptorType, DeviceDescriptor},
    err::{TransferError, USBError},
    host::ControlSetup,
    transfer::{Recipient, Request, RequestType},
};

use super::{EndpointHandle, OwnedRequest};

impl EndpointHandle {
    /// The transfer reads into a buffer of its own, copied to `buff` once it has completed: if this future is
    /// dropped first, the endpoint still owns the buffer the controller may write (unsafe audit F-01).
    pub async fn control_in(
        &mut self,
        param: usb_if::host::ControlSetup,
        buff: &mut [u8],
    ) -> Result<usize, TransferError> {
        let request = OwnedRequest::control_in(param, alloc::vec![0; buff.len()].into_boxed_slice());
        let t = self.wait(request).await?;
        let actual = t.actual_length.min(buff.len());
        buff[..actual].copy_from_slice(&t.buffer[..actual]);
        Ok(t.actual_length)
    }

    /// The transfer sends a copy of `buff` (see `control_in`).
    pub async fn control_out(
        &mut self,
        param: usb_if::host::ControlSetup,
        buff: &[u8],
    ) -> Result<usize, TransferError> {
        let t = self.wait(OwnedRequest::control_out(param, Box::from(buff))).await?;
        Ok(t.actual_length)
    }

    pub async fn set_configuration(
        &mut self,
        configuration_value: u8,
    ) -> Result<(), TransferError> {
        self.control_out(
            ControlSetup {
                request_type: RequestType::Standard,
                recipient: Recipient::Device,
                request: Request::SetConfiguration,
                value: configuration_value as u16,
                index: 0,
            },
            &[],
        )
        .await?;
        Ok(())
    }

    pub async fn get_descriptor(
        &mut self,
        desc_type: DescriptorType,
        desc_index: u8,
        language_id: u16,
        buff: &mut [u8],
    ) -> Result<usize, TransferError> {
        self.control_in(
            ControlSetup {
                request_type: RequestType::Standard,
                recipient: Recipient::Device,
                request: Request::GetDescriptor,
                value: ((desc_type.0 as u16) << 8) | desc_index as u16,
                index: language_id,
            },
            buff,
        )
        .await
    }

    pub async fn get_device_descriptor(&mut self) -> Result<DeviceDescriptor, USBError> {
        let mut buff = alloc::vec![0u8; DeviceDescriptor::LEN];
        let actual = self
            .get_descriptor(DescriptorType::DEVICE, 0, 0, &mut buff)
            .await?;
        if actual != buff.len() {
            return Err(anyhow!(
                "short device descriptor: expected {} bytes, got {actual}",
                buff.len()
            )
            .into());
        }
        trace!("data: {buff:?}");
        let desc = DeviceDescriptor::parse(&buff).ok_or(anyhow!("device descriptor parse err"))?;

        Ok(desc)
    }

    pub async fn get_configuration(&mut self) -> Result<u8, TransferError> {
        let mut buff = alloc::vec![0u8; 1];
        self.control_in(
            ControlSetup {
                request_type: RequestType::Standard,
                recipient: Recipient::Device,
                request: Request::GetConfiguration,
                value: 0,
                index: 0,
            },
            &mut buff,
        )
        .await?;
        Ok(buff[0])
    }

    pub async fn get_configuration_descriptor(
        &mut self,
        index: u8,
    ) -> Result<ConfigurationDescriptor, USBError> {
        let mut header = alloc::vec![0u8; ConfigurationDescriptor::LEN];
        self.get_descriptor(DescriptorType::CONFIGURATION, index, 0, &mut header)
            .await?;

        let total_length = u16::from_le_bytes(header[2..4].try_into().unwrap()) as usize;
        let mut full_data = alloc::vec![0u8; total_length];
        debug!("Reading configuration descriptor for index {index}, total length: {total_length}");
        self.get_descriptor(DescriptorType::CONFIGURATION, index, 0, &mut full_data)
            .await?;

        ConfigurationDescriptor::parse(&full_data)
            .ok_or_else(|| anyhow!("config descriptor parse err").into())
    }
}
