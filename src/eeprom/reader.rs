use crate::{
    eeprom::{types::SiiCategory, Eeprom},
    error::Error,
    timer_factory::TimerFactory,
};

pub struct EepromSectionReader<
    'a,
    const MAX_FRAMES: usize,
    const MAX_PDU_DATA: usize,
    const MAX_SLAVES: usize,
    TIMEOUT,
> {
    start: u16,
    len: u16,
    byte_count: u16,
    read: heapless::Deque<u8, 8>,
    eeprom: &'a Eeprom<'a, MAX_FRAMES, MAX_PDU_DATA, MAX_SLAVES, TIMEOUT>,
    read_length: usize,
}

impl<'a, const MAX_FRAMES: usize, const MAX_PDU_DATA: usize, const MAX_SLAVES: usize, TIMEOUT>
    EepromSectionReader<'a, MAX_FRAMES, MAX_PDU_DATA, MAX_SLAVES, TIMEOUT>
where
    TIMEOUT: TimerFactory,
{
    pub fn new(
        eeprom: &'a Eeprom<'a, MAX_FRAMES, MAX_PDU_DATA, MAX_SLAVES, TIMEOUT>,
        cat: SiiCategory,
    ) -> Self {
        Self {
            eeprom,
            start: cat.start,
            // Category length is given in words (u16) but we're counting bytes here.
            len: cat.len_words * 2,
            byte_count: 0,
            read: heapless::Deque::new(),
            read_length: 0,
        }
    }

    pub async fn next(&mut self) -> Result<Option<u8>, Error> {
        if self.read.is_empty() {
            let read = self.eeprom.read_eeprom_raw(self.start).await?;

            let slice = read.as_slice();

            self.read_length = slice.len();

            for byte in slice.iter() {
                self.read
                    .push_back(*byte)
                    .map_err(|_| Error::EepromSectionOverrun)?;
            }

            self.start += (self.read.len() / 2) as u16;
        }

        let result = self
            .read
            .pop_front()
            .filter(|_| self.byte_count < self.len)
            .map(|byte| {
                self.byte_count += 1;

                byte
            });

        Ok(result)
    }

    pub async fn skip(&mut self, skip: u16) -> Result<(), Error> {
        // TODO: Optimise by calculating new skip address instead of just iterating through chunks
        for _ in 0..skip {
            self.next().await?;
        }

        Ok(())
    }

    pub async fn try_next(&mut self) -> Result<u8, Error> {
        match self.next().await {
            Ok(Some(value)) => Ok(value),
            // TODO: New error type
            Ok(None) => Err(Error::EepromSectionOverrun),
            Err(e) => Err(e),
        }
    }

    pub async fn take_vec<const N: usize>(
        &mut self,
    ) -> Result<Option<heapless::Vec<u8, N>>, Error> {
        self.take_n_vec(N).await
    }

    pub async fn take_vec_exact<const N: usize>(&mut self) -> Result<heapless::Vec<u8, N>, Error> {
        self.take_n_vec(N)
            .await?
            .ok_or(Error::EepromSectionUnderrun)
    }

    pub async fn take_n_vec_exact<const N: usize>(
        &mut self,
        len: usize,
    ) -> Result<heapless::Vec<u8, N>, Error> {
        self.take_n_vec(len)
            .await?
            .ok_or(Error::EepromSectionUnderrun)
    }

    /// Try to take `len` bytes, returning an error if the buffer length `N` is too small.
    pub async fn take_n_vec<const N: usize>(
        &mut self,
        len: usize,
    ) -> Result<Option<heapless::Vec<u8, N>>, Error> {
        let mut buf = heapless::Vec::new();

        let mut count = 0;

        log::trace!(
            "Taking bytes from EEPROM start {}, len {}, N {}",
            self.start,
            len,
            N
        );

        // TODO: Optimise by taking chunks instead of calling next().await until end conditions are satisfied
        loop {
            // We've collected the requested number of bytes
            if count >= len {
                break Ok(Some(buf));
            }

            // If buffer is full, we'd end up with truncated data, so error out.
            if buf.is_full() {
                break Err(Error::EepromSectionOverrun);
            }

            if let Some(byte) = self.next().await? {
                // SAFETY: We check for buffer space using is_full above
                unsafe { buf.push_unchecked(byte) };

                count += 1;
            } else {
                // Not enough data to fill the buffer
                break Ok(None);
            }
        }
    }
}